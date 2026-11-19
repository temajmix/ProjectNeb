//! The handful of words the native bar draws, in every language the application ships.
//!
//! The C# shell keeps these in `.resx` resources and reads them through `Strings.Get`. The Rust bar
//! cannot pay for a resource loader and, more importantly, must be able to format a label into a
//! preallocated UTF-16 buffer while it paints, so the same words live here as a table of
//! `&'static str` behind a small enum. A missing key is a compile error, not a runtime fallback.

/// A language the UI can be drawn in. [`Language::Ru`] is the default, matching [`DEFAULT_LANGUAGE`].
///
/// [`DEFAULT_LANGUAGE`]: crate::config::DEFAULT_LANGUAGE
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Language {
    /// Russian.
    #[default]
    Ru,
    /// English.
    En,
}

impl Language {
    /// Every language the table covers, in the order the settings window should list them.
    pub const ALL: [Self; 2] = [Self::Ru, Self::En];

    /// Reads a BCP 47-ish tag: anything starting with `en` is English, everything else is Russian.
    ///
    /// The tag comes from `config.language`, which is user-editable, so an unknown value must land on
    /// the shipped default rather than fail.
    pub fn from_tag(tag: &str) -> Self {
        if tag.trim().to_ascii_lowercase().starts_with("en") {
            Self::En
        } else {
            Self::Ru
        }
    }

    /// The tag written back into `config.language`.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }

    /// Index into the per-language tables, in [`Language::ALL`] order.
    fn index(self) -> usize {
        match self {
            Self::Ru => 0,
            Self::En => 1,
        }
    }
}

/// One piece of text the UI shows.
///
/// The set is deliberately closed: every entry exists in every language, so a label can never be
/// missing at runtime and the compiler is the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Text {
    /// Label in front of the CPU percentage.
    BarCpu,
    /// Label in front of the memory percentage.
    BarRam,
    /// Tooltip and accessible name of the launcher button.
    BarLauncher,
    /// Tooltip and accessible name of the settings button.
    BarSettings,
    /// Tooltip of the profile name.
    BarProfile,
    /// Name shown when no profile store gave the bar one.
    BarDefaultProfile,
    /// Said by the media widget while nothing is playing.
    BarMedia,
    /// Prefix of the dock's overflow tile and its tooltip: `Ещё 12`.
    DockMore,
    /// Placeholder in the launcher's search box.
    LauncherSearch,
    /// Said when a query matched nothing.
    LauncherEmpty,
    /// The launcher names its data source: the Start Menu and nothing else.
    LauncherScope,
    /// Said while the index is still being built.
    LauncherIndexing,
    /// The launcher's footer: the keys it answers to.
    LauncherHint,
    /// Log line when `Alt+Space` is taken and the fallback gesture wins; `{gesture}` is replaced.
    LauncherHotkeyFallback,
    /// Heading of the memory soak summary.
    SupervisorSoakSummary,
    /// Column heading: a process.
    SupervisorProcess,
    /// Column heading: a process id.
    SupervisorPid,
    /// Column heading: the smallest working set of the run.
    SupervisorMinimum,
    /// Column heading: the largest working set of the run.
    SupervisorMaximum,
    /// Column heading: growth from the first to the last sample.
    SupervisorGrowth,
    /// Line naming the CSV a soak wrote.
    SupervisorSoakFile,
    /// Verdict of `--soak-check` when nothing grew past the limit.
    SupervisorSoakPassed,
    /// Verdict of `--soak-check` when something did.
    SupervisorSoakFailed,
    /// Label in front of a process's own working set.
    SupervisorMemory,
    /// Said when a soak found no ProjectNeb process at all.
    SupervisorNoProcesses,
}

impl Text {
    /// Every key, so a test can walk the whole table.
    pub const ALL: [Self; 25] = [
        Self::BarCpu,
        Self::BarRam,
        Self::BarLauncher,
        Self::BarSettings,
        Self::BarProfile,
        Self::BarDefaultProfile,
        Self::BarMedia,
        Self::DockMore,
        Self::LauncherSearch,
        Self::LauncherEmpty,
        Self::LauncherScope,
        Self::LauncherIndexing,
        Self::LauncherHint,
        Self::LauncherHotkeyFallback,
        Self::SupervisorSoakSummary,
        Self::SupervisorProcess,
        Self::SupervisorPid,
        Self::SupervisorMinimum,
        Self::SupervisorMaximum,
        Self::SupervisorGrowth,
        Self::SupervisorSoakFile,
        Self::SupervisorSoakPassed,
        Self::SupervisorSoakFailed,
        Self::SupervisorMemory,
        Self::SupervisorNoProcesses,
    ];
}

/// The table itself, indexed `[language][key]` in [`Language::ALL`]/[`Text::ALL`] order.
const TABLE: [[&str; Text::ALL.len()]; Language::ALL.len()] = [
    // ru
    [
        "ЦП",
        "ОЗУ",
        "Запуск",
        "Параметры",
        "Профиль",
        "Основной",
        "Ничего не играет",
        "Ещё",
        "Поиск приложений",
        "Ничего не найдено",
        "Ищу только ярлыки меню «Пуск»",
        "Собираю список приложений…",
        "↑↓ выбрать · Enter запустить · Esc закрыть",
        "Alt+Space занят другой программой. Лаунчер открывается по {gesture}.",
        "Итог замера памяти",
        "Процесс",
        "PID",
        "Минимум",
        "Максимум",
        "Рост",
        "Файл",
        "Замер пройден: рост в пределах 20 %",
        "Замер провален: процесс вырос больше чем на 20 %",
        "Память",
        "Подходящих процессов нет",
    ],
    // en
    [
        "CPU",
        "RAM",
        "Launcher",
        "Settings",
        "Profile",
        "Main",
        "Nothing playing",
        "More",
        "Search apps",
        "Nothing found",
        "Start Menu shortcuts only",
        "Building the app list…",
        "↑↓ select · Enter launch · Esc close",
        "Alt+Space is taken by another program. The launcher opens on {gesture}.",
        "Memory soak summary",
        "Process",
        "PID",
        "Minimum",
        "Maximum",
        "Growth",
        "File",
        "Soak check passed: growth within 20 %",
        "Soak check failed: a process grew by more than 20 %",
        "Memory",
        "No matching processes",
    ],
];

/// Weekday names, Monday first, as the bar capitalises them.
const WEEKDAYS: [[&str; 7]; Language::ALL.len()] = [
    [
        "Понедельник",
        "Вторник",
        "Среда",
        "Четверг",
        "Пятница",
        "Суббота",
        "Воскресенье",
    ],
    [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ],
];

/// Month names, January first. The Russian forms are genitive, which is what `d MMMM` produces and
/// what a date such as `16 ноября` needs.
const MONTHS: [[&str; 12]; Language::ALL.len()] = [
    [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ],
    [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ],
];

/// The word for `text` in `language`.
pub fn get(language: Language, text: Text) -> &'static str {
    TABLE[language.index()][text as usize]
}

/// The weekday name for `weekday`, `0` being Monday.
///
/// Out-of-range input cannot happen from a `SYSTEMTIME`, but returning `""` instead of panicking
/// keeps a broken clock from taking the process down.
pub fn weekday(language: Language, weekday: u32) -> &'static str {
    WEEKDAYS[language.index()]
        .get(weekday as usize)
        .copied()
        .unwrap_or("")
}

/// The month name for `month`, `1` being January; `""` for an out-of-range month.
pub fn month(language: Language, month: u32) -> &'static str {
    match month.checked_sub(1) {
        Some(index) => MONTHS[language.index()]
            .get(index as usize)
            .copied()
            .unwrap_or(""),
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_exists_in_every_language() {
        for language in Language::ALL {
            for text in Text::ALL {
                let value = get(language, text);
                assert!(!value.is_empty(), "{language:?}/{text:?} must have a word");
            }
        }
    }

    #[test]
    fn the_two_languages_actually_differ() {
        assert_ne!(
            get(Language::Ru, Text::BarCpu),
            get(Language::En, Text::BarCpu)
        );
        assert_eq!(get(Language::En, Text::BarCpu), "CPU");
        assert_eq!(get(Language::En, Text::BarRam), "RAM");
    }

    #[test]
    fn a_tag_decides_the_language_and_an_unknown_tag_is_russian() {
        assert_eq!(Language::from_tag("en"), Language::En);
        assert_eq!(Language::from_tag("EN-us"), Language::En);
        assert_eq!(Language::from_tag("ru"), Language::Ru);
        assert_eq!(Language::from_tag("de"), Language::Ru);
        assert_eq!(Language::from_tag(""), Language::Ru);
        assert_eq!(Language::default(), Language::Ru);
    }

    #[test]
    fn the_tags_round_trip() {
        for language in Language::ALL {
            assert_eq!(Language::from_tag(language.tag()), language);
        }
    }

    #[test]
    fn weekdays_start_on_monday() {
        assert_eq!(weekday(Language::Ru, 0), "Понедельник");
        assert_eq!(weekday(Language::Ru, 6), "Воскресенье");
        assert_eq!(weekday(Language::En, 0), "Monday");
        assert_eq!(weekday(Language::En, 7), "");
    }

    #[test]
    fn months_are_one_based_and_russian_uses_the_genitive() {
        assert_eq!(month(Language::Ru, 11), "ноября");
        assert_eq!(month(Language::En, 11), "November");
        assert_eq!(month(Language::En, 1), "January");
        assert_eq!(month(Language::Ru, 0), "");
        assert_eq!(month(Language::Ru, 13), "");
    }

    #[test]
    fn the_launcher_strings_name_their_scope_and_their_keys() {
        assert_eq!(
            get(Language::Ru, Text::LauncherScope),
            "Ищу только ярлыки меню «Пуск»"
        );
        assert_eq!(
            get(Language::En, Text::LauncherScope),
            "Start Menu shortcuts only"
        );
        assert!(get(Language::Ru, Text::LauncherEmpty).contains("не найдено"));
        assert_eq!(get(Language::En, Text::LauncherEmpty), "Nothing found");
        assert!(get(Language::Ru, Text::LauncherHint).contains("Esc"));
        assert!(get(Language::En, Text::LauncherHint).contains("Enter"));
    }
}
