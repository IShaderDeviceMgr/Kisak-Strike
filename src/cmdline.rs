//! Command-line parsing.
//!
//! Replaces Valve's `ICommandLine`/`CCommandLine` (`public/tier0/icommandline.h`,
//! `tier0/commandline.cpp`). The *semantics* are preserved because the engine
//! and game code depend on them (`-game csgo`, `+map foo`, "value is the next
//! token" lookup); the *shape* is not — this is a plain owned struct passed
//! explicitly, not a process-wide `CommandLine()` singleton behind a pure
//! virtual interface. See ../../PORTING.md's "What idiomatic means concretely".

/// The parsed process command line.
///
/// Argument matching is ASCII-case-insensitive, matching Source's `Q_stricmp`
/// behavior in `CCommandLine::FindParm`.
#[derive(Debug, Clone, Default)]
pub struct CommandLine {
    args: Vec<String>,
}

impl CommandLine {
    /// Builds from this process's arguments, including argv[0].
    pub fn from_env() -> Self {
        Self {
            args: std::env::args().collect(),
        }
    }

    /// Builds from an explicit argument list (argv[0] included).
    #[allow(dead_code)] // used by tests; will be used for `-basedir`-style re-exec
    pub fn from_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.args.iter().position(|a| a.eq_ignore_ascii_case(name))
    }

    /// True if `name` appears at all. (Valve: `HasParm`/`CheckParm`.)
    pub fn has(&self, name: &str) -> bool {
        self.position(name).is_some()
    }

    /// The token following `name`, if it is a value rather than another
    /// switch. (Valve: `ParmValue`.)
    ///
    /// Returns `None` when `name` is absent, is the final token, **or is
    /// followed by another `-`/`+` argument** — `CCommandLine::ParmValue`
    /// (`tier0/commandline.cpp:646`): "*Probably another cmdline parameter
    /// instead of a valid arg if it starts with '+' or '-'*".
    ///
    /// That last clause is load-bearing rather than cosmetic. `stuffcmds`
    /// walks the arguments skipping each `-switch` **and its value**, so
    /// without it `-window +map sp_a1_intro1` has `-window` swallow `+map` and
    /// the map never loads. See `engine::console`'s
    /// `a_valueless_option_does_not_eat_the_next_command`.
    pub fn value(&self, name: &str) -> Option<&str> {
        let idx = self.position(name)?;
        self.args
            .get(idx + 1)
            .map(String::as_str)
            .filter(|value| !value.starts_with('-') && !value.starts_with('+'))
    }

    /// `value()` with a fallback.
    pub fn value_or<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        self.value(name).unwrap_or(default)
    }

    /// Removes every occurrence of `name` along with its following value token.
    pub fn remove(&mut self, name: &str) {
        while let Some(idx) = self.position(name) {
            // Remove the value first if there is one, so indices stay valid.
            if idx + 1 < self.args.len() {
                self.args.remove(idx + 1);
            }
            self.args.remove(idx);
        }
    }

    /// Appends `name`, optionally followed by `value`.
    pub fn append(&mut self, name: &str, value: Option<&str>) {
        self.args.push(name.to_owned());
        if let Some(value) = value {
            self.args.push(value.to_owned());
        }
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Steam's `-applaunch` inserts its own `-game` argument, which can shadow
    /// a mod's. Keep only the last one when several were given.
    ///
    /// Port of `RemoveSpuriousGameParameters` (`launcher/launcher.cpp:1217`).
    /// The original re-appended the value wrapped in literal quotes; that was
    /// an artifact of its string handling and is dropped here, since nothing
    /// downstream wants the quotes.
    pub fn dedup_game_parm(&mut self) {
        let occurrences = self
            .args
            .iter()
            .filter(|a| a.eq_ignore_ascii_case("-game"))
            .count();
        if occurrences <= 1 {
            return;
        }

        let last = self
            .args
            .iter()
            .enumerate()
            .filter(|(_, a)| a.eq_ignore_ascii_case("-game"))
            .next_back()
            .and_then(|(i, _)| self.args.get(i + 1))
            .cloned();

        self.remove("-game");
        if let Some(value) = last {
            self.append("-game", Some(&value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cl(args: &[&str]) -> CommandLine {
        CommandLine::from_args(args.iter().copied())
    }

    #[test]
    fn finds_parms_case_insensitively() {
        let c = cl(&["game", "-Game", "portal2"]);
        assert!(c.has("-game"));
        assert_eq!(c.value("-GAME"), Some("portal2"));
    }

    #[test]
    fn missing_and_valueless_parms_are_none() {
        let c = cl(&["game", "-novid", "-game"]);
        assert!(c.has("-novid"));
        assert_eq!(
            c.value("-novid"),
            None,
            "a following switch is another parameter, not this one's value"
        );
        assert_eq!(c.value("-game"), None, "trailing parm has no value");
        assert_eq!(c.value("-absent"), None);
        assert_eq!(c.value_or("-absent", "fallback"), "fallback");
    }

    /// The clause `stuffcmds` depends on: a switch with no value must not eat
    /// the `+command` that follows it.
    #[test]
    fn a_switch_is_never_read_as_another_switch_s_value() {
        let c = cl(&["game", "-window", "+map", "sp_a1_intro1"]);
        assert_eq!(c.value("-window"), None);
        assert_eq!(c.value("+map"), Some("sp_a1_intro1"));
    }

    #[test]
    fn remove_takes_the_value_with_it() {
        let mut c = cl(&["game", "-game", "portal2", "-novid"]);
        c.remove("-game");
        assert_eq!(c.args(), &["game", "-novid"]);
    }

    #[test]
    fn append_with_and_without_value() {
        let mut c = cl(&["game"]);
        c.append("-game", Some("portal2"));
        c.append("-insecure", None);
        assert_eq!(c.args(), &["game", "-game", "portal2", "-insecure"]);
    }

    #[test]
    fn dedup_game_parm_keeps_the_last() {
        let mut c = cl(&["game", "-game", "csgo", "-novid", "-game", "portal2"]);
        c.dedup_game_parm();
        assert_eq!(c.value("-game"), Some("portal2"));
        assert_eq!(
            c.args().iter().filter(|a| *a == "-game").count(),
            1,
            "exactly one -game survives"
        );
        assert!(c.has("-novid"), "unrelated parms are untouched");
    }

    #[test]
    fn dedup_game_parm_is_a_noop_for_a_single_occurrence() {
        let mut c = cl(&["game", "-game", "portal2"]);
        c.dedup_game_parm();
        assert_eq!(c.args(), &["game", "-game", "portal2"]);
    }
}
