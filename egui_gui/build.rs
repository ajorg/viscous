//! Puts the logo into the Windows executable, where the shell looks for it.
//!
//! The running window wears the logo because `main` hands it to eframe; this
//! is for everywhere the program is *not* running — Explorer, the Start menu,
//! a shortcut on a desktop, the taskbar button of a pinned copy. Windows takes
//! that icon from the executable's own resources and nowhere else, so it has
//! to be built into the file at link time.
//!
//! An .ico is a bundle of sizes rather than one image, and Windows picks from
//! it: 16px in a list view, 32px on the desktop, 256px in a large-icon folder.
//! They are made here from the one PNG, so the logo has a single source and
//! there is no second file to keep in step with it.
//!
//! Everything below is best effort. A missing resource compiler leaves the
//! executable without its shell icon and says so, which is a blemish; failing
//! the build over it would be worse.

use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The sizes Windows asks for. The file holds them all and the shell picks:
/// 16px in a list, 32px on the desktop, 256px in a large-icon folder.
const SIZES: [u32; 6] = [16, 32, 48, 64, 128, 256];

/// The logo, relative to this crate.
const LOGO: &str = "../viscous.png";

fn main() {
    println!("cargo:rerun-if-changed={LOGO}");
    println!("cargo:rerun-if-changed=build.rs");

    // A resource is a Windows idea; every other target ignores all of this.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = match build_icon() {
        Ok(icon) => icon,
        Err(error) => {
            println!("cargo:warning=no icon built from {LOGO}: {error}");
            return;
        }
    };

    use_llvm_rc_when_cross_compiling();

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(&icon.to_string_lossy());
    // What the shell and the task manager call this, which otherwise comes
    // out as the crate name: nobody running it thinks of it as "egui_gui".
    resource.set("ProductName", "Viscous");
    // Plain ASCII, unlike the prose everywhere else here: this string is read
    // back by whichever resource compiler ran, and they disagree about what
    // encoding a .rc file is in. An em dash is not worth a mojibake risk.
    resource.set("FileDescription", "Viscous - VISCA camera control");
    if let Err(error) = resource.compile() {
        println!(
            "cargo:warning=the executable has no icon of its own: {error}. \
             A resource compiler is what does that — llvm-rc on this side of \
             the fence, rc.exe with the Windows SDK — and one on PATH, or \
             named by RC_PATH, is all this needs."
        );
    }
}

/// Writes the logo out as a multi-size .ico beside the other build products,
/// returning where it went.
fn build_icon() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let logo = image::open(LOGO)?.into_rgba8();

    let mut icon = ico::IconDir::new(ico::ResourceType::Icon);
    for size in SIZES {
        // Lanczos rather than something cheaper: the 16px entry is the logo
        // reduced to a smudge if it's resampled badly, and it is the size the
        // eye meets most often.
        let scaled =
            image::imageops::resize(&logo, size, size, image::imageops::FilterType::Lanczos3);
        let entry = ico::IconImage::from_rgba_data(size, size, scaled.into_raw());
        icon.add_entry(ico::IconDirEntry::encode(&entry)?);
    }

    let path = PathBuf::from(env::var("OUT_DIR")?).join("viscous.ico");
    icon.write(File::create(&path)?)?;
    Ok(path)
}

/// Points `winresource` at a resource compiler when building for Windows from
/// somewhere else.
///
/// It looks for `llvm-rc` on PATH, which is where a Windows host's `rc.exe`
/// effectively is but where LLVM's own is often not: the distribution
/// packages keep it in the versioned directory of the toolchain it belongs to.
/// Finding it there saves every build of this crate from needing a PATH set up
/// for it first.
fn use_llvm_rc_when_cross_compiling() {
    if !cfg!(unix) || env::var_os("RC_PATH").is_some() || on_path("llvm-rc") {
        return;
    }
    let Some(found) = llvm_directories().find_map(|directory| {
        let candidate = directory.join("bin/llvm-rc");
        candidate.is_file().then_some(candidate)
    }) else {
        return;
    };
    // Safe here in the way it isn't in a program: a build script is one
    // thread, and nothing else is reading the environment as this runs.
    unsafe { env::set_var("RC_PATH", found) };
}

fn on_path(program: &str) -> bool {
    env::var_os("PATH").is_some_and(|path| {
        env::split_paths(&path).any(|directory| directory.join(program).is_file())
    })
}

/// Every installed LLVM, newest first: `/usr/lib/llvm-19` and its neighbours.
fn llvm_directories() -> impl Iterator<Item = PathBuf> {
    let mut found: Vec<PathBuf> = Path::new("/usr/lib")
        .read_dir()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("llvm-"))
        })
        .collect();
    found.sort();
    found.into_iter().rev()
}
