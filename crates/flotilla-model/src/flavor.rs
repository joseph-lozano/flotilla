//! Release and dev builds keep apart, so a dev build can run beside the release app on the
//! same boat account: each has its own database, Keychain item, thread VMs (by label) and
//! project environments. Debug builds are dev unless `FLOTILLA_DEV=0`; `FLOTILLA_DEV=1`
//! makes a release build dev.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Release,
    Dev,
}

static FLAVOR: OnceLock<Flavor> = OnceLock::new();

/// Pick the flavor once, at startup, before anything reads it: `debug` is the binary's own
/// `cfg!(debug_assertions)` (this crate's says nothing about the app's).
pub fn init(debug: bool) -> Flavor {
    let dev = match std::env::var("FLOTILLA_DEV").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => debug,
    };
    *FLAVOR.get_or_init(|| if dev { Flavor::Dev } else { Flavor::Release })
}

/// Release until [`init`] says otherwise (tests, and anything that never calls it).
pub fn get() -> Flavor {
    FLAVOR.get().copied().unwrap_or(Flavor::Release)
}

pub fn is_dev() -> bool {
    get() == Flavor::Dev
}

/// A dev build's thread VM names start with this.
const DEV_MARK: &str = "dev · ";

impl Flavor {
    /// The app's name: its data folder, its window and menu, its setup VMs.
    pub fn app_name(self) -> &'static str {
        match self {
            Flavor::Release => "Flotilla",
            Flavor::Dev => "Flotilla Dev",
        }
    }

    /// The value of the `flotilla` label on this build's thread VMs. A full sync takes only
    /// these, so neither build picks up the other's threads.
    pub fn vm_label(self) -> &'static str {
        match self {
            Flavor::Release => "1",
            Flavor::Dev => "dev",
        }
    }

    /// What new project environments' names start with.
    pub fn environment_prefix(self) -> &'static str {
        match self {
            Flavor::Release => "flotilla",
            Flavor::Dev => "flotilla-dev",
        }
    }

    /// The URL scheme this build's app opens (pairing links): `flotilla://`, or
    /// `flotilla-dev://` for a dev build.
    pub fn url_scheme(self) -> &'static str {
        match self {
            Flavor::Release => flotilla_proto::pair::SCHEME,
            Flavor::Dev => flotilla_proto::pair::DEV_SCHEME,
        }
    }

    /// The app's id: its bundle id on a Mac (`script/bundle-macos`); on Linux its `.desktop`
    /// file's name (`script/bundle-linux`), and its window's Wayland app id and X11 class, so
    /// the desktop matches the window to that file.
    pub fn app_id(self) -> &'static str {
        match self {
            Flavor::Release => "com.useflotilla.flotilla",
            Flavor::Dev => "com.useflotilla.flotilla.dev",
        }
    }

    /// The Keychain item holding the boat API key.
    pub fn keychain_service(self) -> &'static str {
        match self {
            Flavor::Release => "flotilla",
            Flavor::Dev => "flotilla-dev",
        }
    }

    /// A thread VM's name on boat: its title, marked in a dev build.
    pub fn vm_name(self, title: &str) -> String {
        match self {
            Flavor::Release => title.to_string(),
            Flavor::Dev => format!("{DEV_MARK}{title}"),
        }
    }

    /// The title in a thread VM's name (without a dev build's mark).
    pub fn title_of(self, vm_name: &str) -> &str {
        match self {
            Flavor::Release => vm_name,
            Flavor::Dev => vm_name.strip_prefix(DEV_MARK).unwrap_or(vm_name),
        }
    }
}

pub fn app_name() -> &'static str {
    get().app_name()
}

pub fn vm_label() -> &'static str {
    get().vm_label()
}

pub fn environment_prefix() -> &'static str {
    get().environment_prefix()
}

pub fn keychain_service() -> &'static str {
    get().keychain_service()
}

pub fn vm_name(title: &str) -> String {
    get().vm_name(title)
}

pub fn title_of(vm_name: &str) -> &str {
    get().title_of(vm_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_vm_names_keep_the_title() {
        assert_eq!(Flavor::Dev.vm_name("Fix the build"), "dev · Fix the build");
        assert_eq!(Flavor::Dev.title_of("dev · Fix the build"), "Fix the build");
        // Renamed outside Flotilla: the name is the title.
        assert_eq!(Flavor::Dev.title_of("Fix the build"), "Fix the build");
        assert_eq!(Flavor::Release.vm_name("dev · x"), "dev · x");
        assert_eq!(Flavor::Release.title_of("dev · x"), "dev · x");
    }
}
