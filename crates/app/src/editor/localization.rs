//! UI translations.
//!
//! Every user-visible editor string is a [`Text`] key resolved with
//! [`Language::text`] at paint time, so switching the language takes effect on the
//! next frame everywhere. To add a string: add a `Text` variant and one arm per
//! language in [`Language::text`]; the exhaustive `match` makes a missing
//! translation a compile error.
//!
//! Node text is not here: it comes from the plugins, which ship their own
//! translations in the node specs, looked up by [`Language::code`].

use v_signal_nodes_core::SocketDirection;

/// A UI language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Language {
    English,
    Russian,
}

/// Key of a translatable string; see [`Language::text`] for the actual strings.
#[derive(Clone, Copy, Debug)]
pub(super) enum Text {
    Language,
    Tools,
    Settings,
    Nodes,
    Debug,
    Ready,
    SelectedSourceSocket,
    Input,
    Output,
    DeleteNode,
    NodeLimitReached,
    ConnectionCreatesCycle,
    ConnectionIncompatible,
    SnapToGrid,
    Scale,
    // Menu bar titles and commands (see `commands`).
    MenuEdit,
    MenuView,
    MenuRun,
    Copy,
    Paste,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    RunRealtime,
    RunAsync,
    // Toggle parameter values, the same for every plugin.
    On,
    Off,
}

impl From<SocketDirection> for Text {
    /// "Input"/"Output", the name of a socket side.
    fn from(direction: SocketDirection) -> Self {
        match direction {
            SocketDirection::Input => Self::Input,
            SocketDirection::Output => Self::Output,
        }
    }
}

impl Language {
    /// All languages, in the order shown in the toolbar picker.
    pub(super) const ALL: [Self; 2] = [Self::English, Self::Russian];

    /// The language's name in itself ("Русский", not "Russian"), for the language picker.
    pub(super) fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Russian => "Русский",
        }
    }

    /// Language code used by plugin translations (`NodeSpec::label`), e.g. `"ru"`.
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Russian => "ru",
        }
    }

    /// The string for `key` in this language.
    pub(super) fn text(self, key: Text) -> &'static str {
        match (self, key) {
            (Self::English, Text::Language) => "Language",
            (Self::English, Text::Tools) => "Tools",
            (Self::English, Text::Settings) => "Settings",
            (Self::English, Text::Nodes) => "Nodes",
            (Self::English, Text::Debug) => "Debug",
            (Self::English, Text::Ready) => "Ready",
            (Self::English, Text::SelectedSourceSocket) => "Selected source socket: node",
            (Self::English, Text::Input) => "Input",
            (Self::English, Text::Output) => "Output",
            (Self::English, Text::DeleteNode) => "Delete node",
            (Self::English, Text::NodeLimitReached) => "Node limit reached",
            (Self::English, Text::ConnectionCreatesCycle) => {
                "Connection refused: it would create a cycle"
            }
            (Self::English, Text::ConnectionIncompatible) => {
                "Connection refused: the sockets don't fit (rate or channels)"
            }
            (Self::English, Text::SnapToGrid) => "Snap to grid",
            (Self::English, Text::Scale) => "Scale",
            (Self::English, Text::MenuEdit) => "Edit",
            (Self::English, Text::MenuView) => "View",
            (Self::English, Text::MenuRun) => "Run",
            (Self::English, Text::Copy) => "Copy",
            (Self::English, Text::Paste) => "Paste",
            (Self::English, Text::ZoomIn) => "Zoom in",
            (Self::English, Text::ZoomOut) => "Zoom out",
            (Self::English, Text::ResetZoom) => "Actual size",
            (Self::English, Text::RunRealtime) => "Run in real time",
            (Self::English, Text::RunAsync) => "Run once (async)",
            (Self::English, Text::On) => "on",
            (Self::English, Text::Off) => "off",
            (Self::Russian, Text::Language) => "Язык",
            (Self::Russian, Text::Tools) => "Инструменты",
            (Self::Russian, Text::Settings) => "Настройки",
            (Self::Russian, Text::Nodes) => "Ноды",
            (Self::Russian, Text::Debug) => "Отладка",
            (Self::Russian, Text::Ready) => "Готово",
            (Self::Russian, Text::SelectedSourceSocket) => "Выбран выходной сокет ноды",
            (Self::Russian, Text::Input) => "Вход",
            (Self::Russian, Text::Output) => "Выход",
            (Self::Russian, Text::DeleteNode) => "Удалить ноду",
            (Self::Russian, Text::NodeLimitReached) => "Достигнут лимит нод",
            (Self::Russian, Text::ConnectionCreatesCycle) => {
                "Соединение отклонено: образуется цикл"
            }
            (Self::Russian, Text::ConnectionIncompatible) => {
                "Соединение отклонено: сокеты несовместимы (частота или каналы)"
            }
            (Self::Russian, Text::SnapToGrid) => "Привязать к сетке",
            (Self::Russian, Text::Scale) => "Масштаб",
            (Self::Russian, Text::MenuEdit) => "Правка",
            (Self::Russian, Text::MenuView) => "Вид",
            (Self::Russian, Text::MenuRun) => "Запуск",
            (Self::Russian, Text::Copy) => "Копировать",
            (Self::Russian, Text::Paste) => "Вставить",
            (Self::Russian, Text::ZoomIn) => "Увеличить",
            (Self::Russian, Text::ZoomOut) => "Уменьшить",
            (Self::Russian, Text::ResetZoom) => "Реальный размер",
            (Self::Russian, Text::RunRealtime) => "Запуск в реальном времени",
            (Self::Russian, Text::RunAsync) => "Однократный запуск (асинхронно)",
            (Self::Russian, Text::On) => "вкл",
            (Self::Russian, Text::Off) => "выкл",
        }
    }
}
