//! Turning a cvar or a command into the lines the list commands print.
//!
//! `ConVar_PrintDescription` (`tier1/convar.cpp:1441`) and `PrintCvar` /
//! `PrintCommand` (`engine/cvar.cpp:851`, `:905`) — the three formatters that
//! `help`, `find`, `differences`, `toggle` and `cvarlist` share. They live
//! together here because they share one thing that is easy to get
//! inconsistently wrong: **the flag table**.
//!
//! # One flag table where the original has three
//!
//! The same six flags are spelled three different ways in the C++:
//! `g_ConVarFlags` (`engine/cvar.cpp:803`) carries an upper-case name for the
//! CSV and a short one for the `cvarlist` column; `g_PrintConVarFlags`
//! (`tier1/convar.cpp:1392`) carries a *lower-case* long name for
//! `ConVar_PrintDescription`, and lists a different subset. That is three
//! encodings of one fact. [`FLAGS`] is the one table, with a long name and a
//! short one, and both spellings match the original's where the original has
//! them.
//!
//! The subsets differ too, and the union is kept: `g_PrintConVarFlags` omits
//! `NEVER_AS_STRING`, `DEVELOPMENTONLY` and `HIDDEN`, so Valve's `help` on a
//! hidden cvar does not say it is hidden. Here it does.

use super::cvar::{CommandFlags, Cvar, CvarFlags};
use super::CommandSpec;

/// How wide `ConVar_PrintDescription` pads before the help text: `%-80s`.
const DESCRIPTION_WIDTH: usize = 80;

/// How much help text it prints: `%.80s`.
const DESCRIPTION_HELP_CHARS: usize = 80;

/// One printable flag: the long name (`help`, the CSV header) and the short
/// one (`cvarlist`'s third column).
struct FlagName {
    long: &'static str,
    short: &'static str,
}

/// The printable flags, in the order every listing shows them.
///
/// Order is `g_ConVarFlags`', minus the twenty-two flags this port does not
/// have (`ENGINE_CONSOLE.md` §4.6) and plus `HIDDEN`, which Valve prints in no
/// listing at all.
const FLAGS: [FlagName; 6] = [
    FlagName {
        long: "archive",
        short: "a",
    },
    FlagName {
        long: "singleplayer",
        short: "sp",
    },
    FlagName {
        long: "cheat",
        short: "cheat",
    },
    FlagName {
        long: "numeric",
        short: "numeric",
    },
    FlagName {
        long: "dev_only",
        short: "dev_only",
    },
    FlagName {
        long: "hidden",
        short: "hidden",
    },
];

/// Which of [`FLAGS`] a cvar has, positionally.
fn cvar_bits(flags: CvarFlags) -> [bool; FLAGS.len()] {
    [
        flags.contains(CvarFlags::ARCHIVE),
        flags.contains(CvarFlags::SPONLY),
        flags.contains(CvarFlags::CHEAT),
        flags.contains(CvarFlags::NEVER_AS_STRING),
        flags.contains(CvarFlags::DEVELOPMENTONLY),
        flags.contains(CvarFlags::HIDDEN),
    ]
}

/// Which of [`FLAGS`] a command has, positionally.
///
/// `ARCHIVE` and `NEVER_AS_STRING` are about a *value*, so a command can never
/// have them — which is why [`CommandFlags`] does not define them and why
/// those two columns are always empty here.
fn command_bits(flags: CommandFlags) -> [bool; FLAGS.len()] {
    [
        false,
        flags.contains(CommandFlags::SPONLY),
        flags.contains(CommandFlags::CHEAT),
        false,
        flags.contains(CommandFlags::DEVELOPMENTONLY),
        flags.contains(CommandFlags::HIDDEN),
    ]
}

/// `ConVar_AppendFlags` (`tier1/convar.cpp:1412`): each set flag as
/// ` <longname>`, appended to the description line.
fn append_long_flags(out: &mut String, bits: [bool; FLAGS.len()]) {
    for (flag, set) in FLAGS.iter().zip(bits) {
        if set {
            out.push(' ');
            out.push_str(flag.long);
        }
    }
}

/// `ConVar_PrintDescription`'s tail: pad to eighty columns, then ` - ` and the
/// first eighty characters of the help text.
///
/// **Divergence, cosmetic:** Valve pads to eighty even when there is no help,
/// leaving trailing spaces on the line. Nothing follows them, so they are
/// dropped here.
fn with_help(mut line: String, help: &str) -> String {
    if help.is_empty() {
        return line;
    }
    let help: String = help.chars().take(DESCRIPTION_HELP_CHARS).collect();
    while line.chars().count() < DESCRIPTION_WIDTH {
        line.push(' ');
    }
    line.push_str(" - ");
    line.push_str(&help);
    line
}

/// The value as `ConVar_PrintDescription` shows it.
///
/// `FCVAR_NEVER_AS_STRING` does not maintain the string form, so those print a
/// *number* — as an integer when the float is integral, which is what keeps
/// `developer` from reading `1.000000`.
///
/// **Use this rather than [`Cvar::string`] anywhere a cvar's value is compared
/// or displayed.** The two differ for exactly the `FCVAR_NEVER_AS_STRING`
/// cvars, and there the string is a stale copy of the declared default that no
/// set ever updates — so `differences` would never list one and `toggle` could
/// never find one in its value list. Valve has the same split and lands on the
/// other side of it: `ConVar::GetString` returns the literal string
/// `"FCVAR_NEVER_AS_STRING"` for those (`public/tier1/convar.h:620`), so its
/// `differences` lists every one of them, always.
pub fn value(cvar: &Cvar) -> String {
    if !cvar.flags().contains(CvarFlags::NEVER_AS_STRING) {
        return cvar.string().to_string();
    }
    let (int, float) = (cvar.int(), cvar.float());
    match (int as f32 - float).abs() < 0.000_001 {
        true => int.to_string(),
        false => format!("{float:.6}"),
    }
}

/// `ConVar_PrintDescription` for a cvar (`tier1/convar.cpp:1441`).
///
/// ```text
/// "sensitivity" = "3" ( def. "1" ) min. 0.000100 max. 10000.000000 archive - Mouse sensitivity.
/// ```
///
/// The default is shown only when it differs, compared **case-insensitively**
/// (`V_stricmp`) so that a string cvar reset by retyping its default in another
/// case still reads as unchanged.
pub fn cvar(cvar: &Cvar) -> String {
    let mut line = format!("\"{}\" = \"{}\"", cvar.name(), value(cvar));

    if !is_at_default(cvar) {
        line.push_str(&format!(" ( def. \"{}\" )", cvar.default_value()));
    }

    let (min, max) = cvar.bounds();
    if let Some(min) = min {
        line.push_str(&format!(" min. {min:.6}"));
    }
    if let Some(max) = max {
        line.push_str(&format!(" max. {max:.6}"));
    }

    append_long_flags(&mut line, cvar_bits(cvar.flags()));
    with_help(line, cvar.help())
}

/// Whether the cvar still holds what it was declared with.
///
/// The predicate `differences` selects on, and the same one that decides
/// whether [`cvar`] prints its `( def. "…" )` clause — one function so that a
/// cvar can never be listed as differing and then shown without the clause
/// saying how.
pub fn is_at_default(cvar: &Cvar) -> bool {
    let held = value(cvar);
    if held.eq_ignore_ascii_case(cvar.default_value()) {
        return true;
    }

    // An `FCVAR_NEVER_AS_STRING` cvar renders its value as a number while its
    // declared default is text, so `3.5` and `3.500000` are one value spelled
    // two ways. Compare those as numbers before calling them different.
    cvar.flags().contains(CvarFlags::NEVER_AS_STRING)
        && super::cvar::atod(&held) == super::cvar::atod(cvar.default_value())
}

/// `ConVar_PrintDescription` for a command, which prints the name and the
/// flags and has no value to show.
pub fn command(spec: &CommandSpec) -> String {
    let mut line = format!("\"{}\" ", spec.name);
    append_long_flags(&mut line, command_bits(spec.flags));
    with_help(line, spec.help)
}

/// `PrintCvar` (`engine/cvar.cpp:851`): one `cvarlist` row.
///
/// ```text
/// sv_cheats                                : 0        : , cheat          : Allow cheat commands and cvars.
/// ```
///
/// The flag column really does open with `", "` — `Q_snprintf(f, ", %s")` runs
/// for the first flag as well as the rest — and it is kept, because this is the
/// output people recognise from the shipped engine.
pub fn cvar_row(cvar: &Cvar) -> String {
    // "Clean up integers": an integral value prints as one rather than as
    // `1.000`.
    let value = match cvar.int() == cvar.float() as i32 {
        true => format!("{:<8}", cvar.int()),
        false => format!("{:<8.3}", cvar.float()),
    };
    row(
        cvar.name(),
        &value,
        &short_flags(cvar_bits(cvar.flags())),
        cvar.help(),
    )
}

/// `PrintCommand` (`engine/cvar.cpp:905`), whose value column is the literal
/// `cmd`.
///
/// **Divergence:** Valve leaves the flag column empty for a command, while its
/// own `help` prints a command's flags — an oversight rather than a decision,
/// and the column is filled here. Seeing which commands are cheat-gated is the
/// point of the listing.
pub fn command_row(spec: &CommandSpec) -> String {
    row(
        spec.name,
        "cmd",
        &short_flags(command_bits(spec.flags)),
        spec.help,
    )
}

/// `"%-40s : %-8s : %-16s : %s"`.
fn row(name: &str, value: &str, flags: &str, help: &str) -> String {
    format!(
        "{name:<40} : {value:<8} : {flags:<16} : {}",
        strip_tabs_and_returns(help)
    )
}

fn short_flags(bits: [bool; FLAGS.len()]) -> String {
    let mut out = String::new();
    for (flag, set) in FLAGS.iter().zip(bits) {
        if set {
            out.push_str(", ");
            out.push_str(flag.short);
        }
    }
    out
}

/// `cvarlist log <file>`'s header row.
///
/// **Divergence:** Valve's `PrintListHeader` emits one stray empty column,
/// because `csvflagstr` already ends in a comma and the format string adds
/// another (`engine/cvar.cpp:827`). The rows carry the same extra comma, so the
/// columns still line up and the bug is invisible — but nothing reads this file
/// back, so it is dropped rather than reproduced.
pub fn csv_header() -> String {
    let mut fields = vec!["Name".to_string(), "Value".to_string()];
    fields.extend(FLAGS.iter().map(|flag| flag.long.to_string()));
    fields.push("Help Text".to_string());
    csv(&fields)
}

pub fn cvar_csv(cvar: &Cvar) -> String {
    let value = match cvar.int() == cvar.float() as i32 {
        true => cvar.int().to_string(),
        false => format!("{:.3}", cvar.float()),
    };
    csv_row(cvar.name(), &value, cvar_bits(cvar.flags()), cvar.help())
}

pub fn command_csv(spec: &CommandSpec) -> String {
    csv_row(spec.name, "cmd", command_bits(spec.flags), spec.help)
}

fn csv_row(name: &str, value: &str, bits: [bool; FLAGS.len()], help: &str) -> String {
    // "Names starting with +/- need to be wrapped in single quotes", which is
    // a spreadsheet reading `+forward` as a formula.
    let name = match name.starts_with(['+', '-']) {
        true => format!("'{name}'"),
        false => name.to_string(),
    };

    let mut fields = vec![name, value.to_string()];
    fields.extend(FLAGS.iter().zip(bits).map(|(flag, set)| match set {
        true => flag.long.to_string(),
        false => String::new(),
    }));
    fields.push(strip_quotes(help));
    csv(&fields)
}

/// Every field quoted, which is `FPrintf`'s `"%s","%s",…` and needs no escaping
/// of its own because [`strip_quotes`] has already run over the only field that
/// can contain one.
fn csv(fields: &[String]) -> String {
    let quoted: Vec<String> = fields.iter().map(|field| format!("\"{field}\"")).collect();
    quoted.join(",")
}

/// `StripTabsAndReturns` (`engine/cvar.cpp:736`): help text goes into a
/// column, so a newline or a tab in it would break the column.
fn strip_tabs_and_returns(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            '"' => '\'',
            other => other,
        })
        .collect()
}

/// `StripQuotes` (`engine/cvar.cpp:769`): the CSV quotes every field, so a
/// quote inside one would end it early.
fn strip_quotes(text: &str) -> String {
    text.replace('"', "'")
}

/// How `cvarlist` orders its rows: `ConCommandBaseLessFunc`
/// (`engine/cvar.cpp:935`) drops a leading `+` or `-` and compares
/// case-insensitively, so that `+forward` and `-forward` sort together under
/// `f` rather than under the punctuation.
///
/// The second element is not Valve's. Its comparator makes `+forward` and
/// `-forward` *equal*, and it inserts them into a red-black tree, so which
/// comes out first is an accident of insertion order — which in Rust would be
/// an accident of `HashMap` iteration order, and so would differ between runs.
pub fn list_order(name: &str) -> (String, String) {
    let stripped = name.strip_prefix(['+', '-']).unwrap_or(name);
    (stripped.to_ascii_lowercase(), name.to_ascii_lowercase())
}

/// How `find` and `differences` order theirs: `ConVarSortFunc`
/// (`vstdlib/cvar.cpp:1044`), a plain caseless compare of the whole name.
pub fn name_order(name: &str) -> String {
    name.to_ascii_lowercase()
}
