//! `cargo xtask dist`: build a release binary and package it for the host
//! OS into `target/dist/`:
//!
//! - Linux: `spiceport-<ver>-<target>.tar.gz`, plus an AppImage when
//!   `appimagetool` is on `PATH`;
//! - macOS: `spiceport-<ver>-<target>.app.zip` (a `spiceport.app` bundle with
//!   the microphone usage description SDL needs) and a bare-binary
//!   `.tar.gz`. With `MACOS_SIGN_IDENTITY` set, both are code-signed with
//!   the hardened runtime and the audio-input entitlement; notarisation is
//!   done by the release workflow;
//! - Windows: `spiceport-<ver>-<target>.zip` with `spiceport.exe`.
//!
//! Every archive also holds README.md and LICENSE.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::root;

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    eprintln!("+ {cmd:?}");
    let st = cmd.status().with_context(|| format!("running {cmd:?}"))?;
    if !st.success() {
        bail!("{cmd:?} failed with {st}");
    }
    Ok(())
}

fn host_target() -> anyhow::Result<String> {
    let out = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .arg("-vV")
        .output()?;
    let s = String::from_utf8(out.stdout)?;
    s.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_string)
        .context("no host in rustc -vV")
}

fn version() -> anyhow::Result<String> {
    let toml = fs::read_to_string(root().join("Cargo.toml"))?;
    toml.lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("version = \"")
                .and_then(|v| v.strip_suffix('"'))
        })
        .map(str::to_string)
        .context("no workspace version")
}

pub fn dist(args: Vec<String>) -> anyhow::Result<()> {
    let mut target = None;
    let mut build = true;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target = Some(it.next().context("--target needs a value")?),
            "--no-build" => build = false,
            other => bail!("unknown dist option {other}"),
        }
    }
    let target = match target {
        Some(t) => t,
        None => host_target()?,
    };
    let root = root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    if build {
        run(Command::new(cargo).current_dir(&root).args([
            "build",
            "--release",
            "--locked",
            "-p",
            "spiceport",
            "--target",
            &target,
        ]))?;
    }
    let exe = if target.contains("windows") {
        "spiceport.exe"
    } else {
        "spiceport"
    };
    let bin = root.join("target").join(&target).join("release").join(exe);
    if !bin.exists() {
        bail!("{} not found", bin.display());
    }
    let ver = version()?;
    let name = format!("spiceport-{ver}-{target}");
    let dist = root.join("target/dist");
    let stage = dist.join(&name);
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    for f in ["README.md", "LICENSE"] {
        fs::copy(root.join(f), stage.join(f))?;
    }
    fs::copy(&bin, stage.join(exe))?;

    if target.contains("windows") {
        let zip = dist.join(format!("{name}.zip"));
        let _ = fs::remove_file(&zip);
        // Not `tar -a`: on CI runners Git's GNU tar can shadow Windows' bsdtar.
        run(Command::new("powershell")
            .current_dir(&dist)
            .args(["-NoProfile", "-Command", "Compress-Archive", "-Path"])
            .arg(&name)
            .arg("-DestinationPath")
            .arg(&zip))?;
        println!("{}", zip.display());
    } else if target.contains("apple") {
        sign_macos(&stage.join(exe), &root)?;
        tarball(&dist, &name)?;
        let app = make_app(&dist, &name, &bin, &ver, &root)?;
        let zip = dist.join(format!("{name}.app.zip"));
        let _ = fs::remove_file(&zip);
        run(Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&app)
            .arg(&zip))?;
        println!("{}", zip.display());
    } else {
        tarball(&dist, &name)?;
        appimage(&dist, &name, &bin, &target, &root)?;
    }
    Ok(())
}

fn tarball(dist: &Path, name: &str) -> anyhow::Result<()> {
    let tgz = dist.join(format!("{name}.tar.gz"));
    run(Command::new("tar")
        .current_dir(dist)
        .arg("-czf")
        .arg(&tgz)
        .arg(name))?;
    println!("{}", tgz.display());
    Ok(())
}

fn sign_macos(path: &Path, root: &Path) -> anyhow::Result<()> {
    let Ok(identity) = std::env::var("MACOS_SIGN_IDENTITY") else {
        eprintln!(
            "MACOS_SIGN_IDENTITY not set: leaving {} unsigned",
            path.display()
        );
        return Ok(());
    };
    run(Command::new("codesign")
        .args([
            "--force",
            "--timestamp",
            "--options",
            "runtime",
            "--sign",
            &identity,
            "--entitlements",
        ])
        .arg(root.join("packaging/macos/entitlements.plist"))
        .arg(path))
}

fn make_app(
    dist: &Path,
    name: &str,
    bin: &Path,
    ver: &str,
    root: &Path,
) -> anyhow::Result<PathBuf> {
    let app = dist.join(format!("{name}-app")).join("spiceport.app");
    let _ = fs::remove_dir_all(app.parent().unwrap());
    let contents = app.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources"))?;
    let plist =
        fs::read_to_string(root.join("packaging/macos/Info.plist"))?.replace("@VERSION@", ver);
    fs::write(contents.join("Info.plist"), plist)?;
    fs::copy(bin, contents.join("MacOS/spiceport"))?;
    fs::copy(root.join("README.md"), contents.join("Resources/README.md"))?;
    fs::copy(root.join("LICENSE"), contents.join("Resources/LICENSE"))?;
    sign_macos(&app, root)?;
    Ok(app)
}

fn appimage(dist: &Path, name: &str, bin: &Path, target: &str, root: &Path) -> anyhow::Result<()> {
    let tool = std::env::var("APPIMAGETOOL").unwrap_or_else(|_| "appimagetool".into());
    if Command::new(&tool).arg("--version").output().is_err() {
        eprintln!("{tool} not found: skipping the AppImage");
        return Ok(());
    }
    let appdir = dist.join(format!("{name}.AppDir"));
    let _ = fs::remove_dir_all(&appdir);
    fs::create_dir_all(appdir.join("usr/bin"))?;
    fs::copy(bin, appdir.join("usr/bin/spiceport"))?;
    fs::copy(
        root.join("packaging/linux/spiceport.desktop"),
        appdir.join("spiceport.desktop"),
    )?;
    fs::copy(
        root.join("packaging/spiceport.svg"),
        appdir.join("spiceport.svg"),
    )?;
    fs::write(
        appdir.join("AppRun"),
        "#!/bin/sh\nexec \"$(dirname \"$(readlink -f \"$0\")\")/usr/bin/spiceport\" \"$@\"\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(appdir.join("AppRun"), fs::Permissions::from_mode(0o755))?;
    }
    let arch = target.split('-').next().unwrap_or("x86_64");
    let out = dist.join(format!("{name}.AppImage"));
    run(Command::new(&tool).env("ARCH", arch).arg(&appdir).arg(&out))?;
    println!("{}", out.display());
    Ok(())
}
