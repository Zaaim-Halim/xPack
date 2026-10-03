//! Running one hook script in `QuickJS`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rquickjs::function::{Opt, Rest};
use rquickjs::{Context, Ctx, Exception, Function, Module, Object, Runtime, Value};
use xpack_core::hooks::Scope;

use crate::policy::Policy;
use crate::request::{Request, program_environment, steers_programs};

/// The most memory a hook's script may use. Generous for a script; a hook
/// that needs more is doing something a program it runs should do.
const MEMORY_LIMIT: usize = 128 * 1024 * 1024;

/// The stack of the thread the engine runs on, and how much of it the engine
/// may use. `QuickJS` recurses on the native stack; with its own limit well
/// inside a stack of known size, runaway recursion is a JavaScript error
/// rather than a crash, on every platform, whatever the size of the main
/// thread's stack (1 MiB on Windows).
const THREAD_STACK: usize = 16 * 1024 * 1024;
const ENGINE_STACK: usize = 4 * 1024 * 1024;

/// How much of each of a program's two outputs a hook is handed.
const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;

/// How a hook ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `main` returned, or its promise resolved.
    Succeeded,
    /// It threw, its promise rejected, or it could not be run, in its words.
    Failed(String),
    /// It was still running at its deadline.
    TimedOut,
}

/// Runs the script `source` of `request`.
pub fn run(request: &Request, source: &str) -> Outcome {
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new()
            .name("hook".into())
            .stack_size(THREAD_STACK)
            .spawn_scoped(scope, || run_here(request, source));
        match spawned {
            Ok(thread) => thread.join().unwrap_or_else(|_| {
                Outcome::Failed("the script engine stopped unexpectedly".into())
            }),
            Err(error) => Outcome::Failed(format!("the script engine could not start: {error}")),
        }
    })
}

/// Whether `source` parses as a hook script, without running any of it.
///
/// What a version is checked with before it is accepted, so a script that
/// cannot run is refused when it arrives rather than failing when it is due.
pub fn check(name: &str, source: &str) -> Result<(), String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    context.with(|ctx| {
        Module::declare(ctx.clone(), name, source).map(drop).map_err(|e| describe(&ctx, &e))
    })
}

fn run_here(request: &Request, source: &str) -> Outcome {
    let policy = match Policy::of(request) {
        Ok(policy) => Rc::new(policy),
        Err(refusal) => return Outcome::Failed(refusal),
    };
    let deadline = Instant::now() + Duration::from_secs(u64::from(request.timeout_seconds));
    let Ok(runtime) = Runtime::new() else {
        return Outcome::Failed("the script engine could not start".into());
    };
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(ENGINE_STACK);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let Ok(context) = Context::full(&runtime) else {
        return Outcome::Failed("the script engine could not start".into());
    };

    let result = context.with(|ctx| evaluate(&ctx, request, source, &policy, deadline));
    if Instant::now() > deadline {
        return Outcome::TimedOut;
    }
    match result {
        Ok(()) => Outcome::Succeeded,
        Err(message) => Outcome::Failed(message),
    }
}

fn evaluate(
    ctx: &Ctx<'_>,
    request: &Request,
    source: &str,
    policy: &Rc<Policy>,
    deadline: Instant,
) -> Result<(), String> {
    let name = request
        .script
        .file_name()
        .map_or_else(|| "hook.js".to_string(), |name| name.to_string_lossy().into_owned());
    let declared = Module::declare(ctx.clone(), name, source).map_err(|e| describe(ctx, &e))?;
    let (module, evaluated) = declared.eval().map_err(|e| describe(ctx, &e))?;
    evaluated.finish::<()>().map_err(|e| describe(ctx, &e))?;
    let main: Function =
        module.get("main").map_err(|_| "the script exports no function main".to_string())?;
    let argument = context_object(ctx, request, policy, deadline).map_err(|e| describe(ctx, &e))?;
    let returned: Value = main.call((argument,)).map_err(|e| describe(ctx, &e))?;
    if let Some(promise) = returned.as_promise() {
        promise.finish::<Value>().map_err(|e| describe(ctx, &e))?;
    }
    Ok(())
}

/// The `ctx` a hook's `main` is given.
fn context_object<'js>(
    ctx: &Ctx<'js>,
    request: &Request,
    policy: &Rc<Policy>,
    deadline: Instant,
) -> rquickjs::Result<Object<'js>> {
    let object = Object::new(ctx.clone())?;
    let point = request.point.to_string();
    let (operation, moment) = point.split_once('.').unwrap_or((point.as_str(), ""));
    object.set("operation", operation)?;
    object.set("when", moment)?;
    object.set("cause", request.cause.clone())?;
    object.set("fromVersion", request.from_version.clone())?;
    object.set("toVersion", request.to_version.clone())?;
    let platform = Object::new(ctx.clone())?;
    if let Ok(host) = xpack_core::Platform::host() {
        platform.set("os", host.os.to_string())?;
        platform.set("arch", host.arch.to_string())?;
    }
    freeze(ctx, &platform)?;
    object.set("platform", platform)?;
    object.set(
        "scope",
        match request.scope {
            Scope::User => "user",
            Scope::Machine => "machine",
        },
    )?;
    for (key, path) in [
        ("applicationDir", Some(&request.application_dir)),
        ("versionDir", Some(&request.version_dir)),
        ("dataDir", Some(&request.data_dir)),
        ("logDir", Some(&request.log_dir)),
        ("tempDir", Some(&request.temp_dir)),
        ("home", request.home.as_ref()),
        ("programData", request.program_data.as_ref()),
    ] {
        if let Some(path) = path {
            object.set(key, path.display().to_string())?;
        }
    }
    let environment = Object::new(ctx.clone())?;
    for (name, value) in &request.environment {
        environment.set(name.as_str(), value.as_str())?;
    }
    freeze(ctx, &environment)?;
    object.set("environment", environment)?;

    object.set(
        "path",
        Function::new(ctx.clone(), |parts: Rest<String>| {
            let mut path = PathBuf::new();
            for part in parts.0 {
                path.push(part);
            }
            path.display().to_string()
        })?,
    )?;

    let log = Object::new(ctx.clone())?;
    // One line each, on standard error, which the runner reads line by line
    // and prefixes with the hook point. Never beginning `xpack-hook: `, which
    // is this program's own word on how the hook ended. Anything may be
    // logged: it is made text, and text JavaScript allows but UTF-8 cannot
    // carry (a lone surrogate) is replaced rather than failing the hook.
    let as_text: Function = ctx.eval("(value) => String(value).toWellFormed()")?;
    for (level, prefix) in [("info", ""), ("warn", "warning: "), ("error", "error: ")] {
        let as_text = as_text.clone();
        log.set(
            level,
            Function::new(ctx.clone(), move |value: Value<'js>| {
                let text: String = as_text.call((value,))?;
                for line in text.lines() {
                    let line = line.strip_prefix("xpack-hook: ").unwrap_or(line);
                    xpack_core::errln!("{prefix}{line}");
                }
                Ok::<_, rquickjs::Error>(())
            })?,
        )?;
    }
    freeze(ctx, &log)?;
    object.set("log", log)?;

    let environment = program_environment(request);
    let version_dir = request.version_dir.clone();
    let exec_policy = Rc::clone(policy);
    object.set(
        "exec",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  program: String,
                  args: Opt<Vec<String>>,
                  options: Opt<Object<'js>>| {
                let run = Exec {
                    policy: &exec_policy,
                    environment: &environment,
                    version_dir: &version_dir,
                    deadline,
                };
                run.call(&ctx, &program, args.0.unwrap_or_default(), options.0)
            },
        )?,
    )?;
    object.set("file", file_object(ctx, policy)?)?;
    freeze(ctx, &object)?;
    Ok(object)
}

/// What `ctx.exec` runs programs with.
struct Exec<'a> {
    policy: &'a Policy,
    environment: &'a BTreeMap<String, String>,
    version_dir: &'a Path,
    deadline: Instant,
}

impl Exec<'_> {
    /// Runs a program the package declared, directly, never through a shell.
    fn call<'js>(
        &self,
        ctx: &Ctx<'js>,
        program: &str,
        args: Vec<String>,
        options: Option<Object<'js>>,
    ) -> rquickjs::Result<Object<'js>> {
        let throw = |message: String| Exception::throw_message(ctx, &message);
        let file = self.policy.program(program).map_err(throw)?;
        let mut command = Command::new(&file);
        command.args(args).env_clear().envs(self.environment);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut cwd = self.version_dir.to_path_buf();
        let mut limit = self.deadline;
        if let Some(options) = options {
            if let Some(dir) = options.get::<_, Option<String>>("cwd")? {
                cwd = self.policy.may_read(Path::new(&dir)).map_err(throw)?;
            }
            if let Some(extra) = options.get::<_, Option<Object<'js>>>("env")? {
                for key in extra.keys::<String>() {
                    let key = key?;
                    if steers_programs(&key) {
                        return Err(throw(format!(
                            "{key} would change which code {program} runs; a hook may not set it"
                        )));
                    }
                    let value: String = extra.get(key.as_str())?;
                    command.env(key, value);
                }
            }
            if let Some(seconds) = options.get::<_, Option<u32>>("timeoutSeconds")? {
                limit = limit.min(Instant::now() + Duration::from_secs(u64::from(seconds)));
            }
        }
        command.current_dir(cwd);
        xpack_platform::without_a_console(&mut command);

        let mut child =
            command.spawn().map_err(|e| throw(format!("{program} could not be started: {e}")))?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let out_of_time = || throw(format!("{program} ran out of time"));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() > limit => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(out_of_time());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(throw(e.to_string())),
            }
        };
        // The program has exited, but something it started may still hold
        // its output open; the deadline holds for that wait too.
        let stdout = collect(&stdout, limit).ok_or_else(out_of_time)?;
        let stderr = collect(&stderr, limit).ok_or_else(out_of_time)?;
        let result = Object::new(ctx.clone())?;
        result.set("exitCode", status.code())?;
        result.set("stdout", stdout)?;
        result.set("stderr", stderr)?;
        Ok(result)
    }
}

/// Reads a child's output on a thread of its own, so a full pipe never
/// stalls it, and hands it over when the pipe closes.
///
/// Keeps the first [`OUTPUT_LIMIT`] bytes and reads the rest away: a program
/// that writes without end neither blocks on its pipe nor fills this
/// process's memory, which the engine's limit does not cover.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.by_ref().take(OUTPUT_LIMIT).read_to_end(&mut bytes);
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        }
        let _ = sender.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    receiver
}

/// The output `drain` read, if its pipe closed before `limit`.
fn collect(output: &mpsc::Receiver<String>, limit: Instant) -> Option<String> {
    output.recv_timeout(limit.saturating_duration_since(Instant::now())).ok()
}

/// `ctx.file`: file operations, each held to the policy, each acting on the
/// path the policy checked rather than the one it was given.
fn file_object<'js>(ctx: &Ctx<'js>, policy: &Rc<Policy>) -> rquickjs::Result<Object<'js>> {
    let object = Object::new(ctx.clone())?;
    let fail = |ctx: &Ctx<'js>, message: String| Exception::throw_message(ctx, &message);

    let p = Rc::clone(policy);
    object.set(
        "exists",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
            let path = p.may_read(Path::new(&path)).map_err(|r| fail(&ctx, r))?;
            Ok::<_, rquickjs::Error>(path.exists())
        })?,
    )?;
    let p = Rc::clone(policy);
    object.set(
        "read",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
            let resolved = p.may_read(Path::new(&path)).map_err(|r| fail(&ctx, r))?;
            std::fs::read_to_string(&resolved).map_err(|e| fail(&ctx, format!("{path}: {e}")))
        })?,
    )?;
    let p = Rc::clone(policy);
    object.set(
        "write",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String, text: String| {
            let resolved = p.may_write(Path::new(&path)).map_err(|r| fail(&ctx, r))?;
            std::fs::write(&resolved, text).map_err(|e| fail(&ctx, format!("{path}: {e}")))
        })?,
    )?;
    let p = Rc::clone(policy);
    object.set(
        "copy",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, from: String, to: String| {
            let source = p.may_read(Path::new(&from)).map_err(|r| fail(&ctx, r))?;
            let target = p.may_write(Path::new(&to)).map_err(|r| fail(&ctx, r))?;
            std::fs::copy(&source, &target)
                .map(|_| ())
                .map_err(|e| fail(&ctx, format!("{from} → {to}: {e}")))
        })?,
    )?;
    let p = Rc::clone(policy);
    object.set(
        "move",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, from: String, to: String| {
            let source = p.may_change_entry(Path::new(&from)).map_err(|r| fail(&ctx, r))?;
            let target = p.may_change_entry(Path::new(&to)).map_err(|r| fail(&ctx, r))?;
            std::fs::rename(&source, &target).map_err(|e| fail(&ctx, format!("{from} → {to}: {e}")))
        })?,
    )?;
    let p = Rc::clone(policy);
    object.set(
        "remove",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
            let entry = p.may_change_entry(Path::new(&path)).map_err(|r| fail(&ctx, r))?;
            remove_entry(&entry).map_err(|e| fail(&ctx, format!("{path}: {e}")))
        })?,
    )?;
    let p = Rc::clone(policy);
    object.set(
        "makeDir",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
            let resolved = p.may_make_dir(Path::new(&path)).map_err(|r| fail(&ctx, r))?;
            std::fs::create_dir_all(&resolved).map_err(|e| fail(&ctx, format!("{path}: {e}")))
        })?,
    )?;
    let p = Rc::clone(policy);
    object.set(
        "list",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
            let resolved = p.may_read(Path::new(&path)).map_err(|r| fail(&ctx, r))?;
            let mut names: Vec<String> = std::fs::read_dir(&resolved)
                .map_err(|e| fail(&ctx, format!("{path}: {e}")))?
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Ok::<_, rquickjs::Error>(names)
        })?,
    )?;
    freeze(ctx, &object)?;
    Ok(object)
}

/// Removes the entry at `path`: a directory with everything in it, or a file,
/// or a link, never what a link points to.
fn remove_entry(path: &Path) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if kind.is_symlink() {
        // A link to a directory is a directory entry on Windows.
        std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
    } else if kind.is_dir() {
        // Does not follow links inside the directory either.
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Makes `object` read-only, as JavaScript's own `Object.freeze` does, so a
/// hook cannot swap a checked function for one of its own.
fn freeze<'js>(ctx: &Ctx<'js>, object: &Object<'js>) -> rquickjs::Result<()> {
    let constructor: Object = ctx.globals().get("Object")?;
    let freeze: Function = constructor.get("freeze")?;
    freeze.call::<_, Value>((object.clone(),))?;
    Ok(())
}

/// A JavaScript error, in its own words.
fn describe(ctx: &Ctx<'_>, error: &rquickjs::Error) -> String {
    match error {
        rquickjs::Error::Exception => {
            let thrown = ctx.catch();
            if let Some(exception) = thrown.as_exception() {
                return exception.message().unwrap_or_else(|| "an exception".into());
            }
            if let Some(text) = thrown.as_string().and_then(|s| s.to_string().ok()) {
                return text;
            }
            format!("{thrown:?}")
        }
        // A hook has no timers and no events: a promise still pending once
        // every job has run is waiting on something that can never happen.
        rquickjs::Error::WouldBlock => "it awaits a promise that can never settle".into(),
        other => other.to_string(),
    }
}
