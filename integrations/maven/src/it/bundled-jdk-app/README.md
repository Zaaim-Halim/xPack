# xPack Demo

A Swing application packaged with every setting the plugin has, each explained
in `pom.xml`: identity, launch arguments, a bundled `jlink` runtime, updates
and the prompt, health checks, the desktop entry and its icon, and the
installation wizard.

## Build it

From this directory, with the plugin installed (`mvn install` in
`integrations/maven`) and the xPack binaries built (`cargo build --workspace`
at the repository root):

```sh
# Once: a signing key, kept outside this project.
xpack keygen --out ~/xpack-demo-keys/signing.json

mvn package -Pinstaller \
  -Dxpack.home=<repository>/target/debug \
  -Dxpack.key=$HOME/xpack-demo-keys/signing.json \
  -Dxpack.publicKey=$HOME/xpack-demo-keys/signing.pub.json
```

Without `-Pinstaller` the build makes the package only, which is what every
release needs; the installer, which changes rarely, is built when asked for.
Both are written to `target/xpack/dist`: on macOS
`Install xPack Demo.app`, on Windows `xPack-Demo-1.0.0-windows-x64-Setup.exe`,
on Linux `xPack-Demo-1.0.0-linux-x64-installer`.

## What the installer shows

Opened by a person, it runs the wizard: welcome, the licence in `LICENSE.txt`,
where to install, a summary, progress, and a last page that offers to open the
demo. Run with `--silent` it installs with no window, exactly as the wizard
clicked straight through would.

The icon is `src/xpack/icons/<platform>/`, one format per platform, chosen by
the profiles at the end of `pom.xml`.

## Remove it

```sh
# macOS
"$HOME/Library/Application Support/xpack/com.example.demo/Uninstall xPack Demo" --yes
```
