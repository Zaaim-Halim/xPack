//! What a hook does, recorded for `xpack hooks test`.
//!
//! Under test every program a hook runs, every file change it makes and
//! every refusal it meets is appended to the plan's record, one line of JSON
//! each. In plan mode none of it is done: the record is all there is, and a
//! program the hook runs answers what the test said it would.

use std::io::Write;
use std::path::PathBuf;

use xpack_core::hooks::{HookPoint, Plan, PlanAction, PlanLine, ProgramAnswer};

use crate::request::Request;

/// Records one hook's actions under test.
#[derive(Debug)]
pub(crate) struct Recorder {
    plan: Plan,
    point: HookPoint,
}

impl Recorder {
    /// The recorder `request` asks for, if it is under test.
    pub(crate) fn of(request: &Request) -> Option<Self> {
        request.plan.clone().map(|plan| Self { plan, point: request.point })
    }

    /// Whether what is recorded is also done.
    pub(crate) fn performs(&self) -> bool {
        self.plan.perform
    }

    /// What `program` answers when it is not run.
    pub(crate) fn answer(&self, program: &str) -> ProgramAnswer {
        let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
        self.plan
            .answers
            .get(program)
            .or_else(|| self.plan.answers.get(name))
            .cloned()
            .unwrap_or_default()
    }

    /// Appends `action` to the record. A record that cannot be written
    /// fails the hook: a test whose record is incomplete proves nothing.
    pub(crate) fn note(&self, action: PlanAction, performed: bool) -> Result<(), String> {
        let line = PlanLine {
            version: self.plan.version.clone(),
            point: self.point,
            script: self.plan.script.clone(),
            action,
            performed,
        };
        let mut text = serde_json::to_string(&line).map_err(|e| e.to_string())?;
        text.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.plan.record)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .map_err(|e| format!("{}: {e}", self.plan.record.display()))
    }

    /// Records a refusal, and hands it back to be thrown.
    pub(crate) fn refused(&self, reason: String) -> String {
        let _ = self.note(PlanAction::Refused { reason: reason.clone() }, false);
        reason
    }
}

/// A path as the record keeps it.
pub(crate) fn recorded(path: &std::path::Path) -> PathBuf {
    path.to_path_buf()
}
