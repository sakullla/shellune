//! User-facing copy, in English and Chinese (R12, R18).
//!
//! Every string the shell renders comes from here so that both languages are
//! complete at the same time and no view invents wording inline. The Chinese
//! column is not a literal translation of the English one: it is written for a
//! Chinese reader, and the tests only require that both are present, non-empty
//! and renderable.
//!
//! The product name, pronunciation and the two taglines stay in
//! [`crate::app::product`] because R18 fixes them independently of the UI
//! language.

use shellune_core::{SessionState, TransferState};

/// UI language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    /// English (default).
    #[default]
    English,
    /// Simplified Chinese.
    Chinese,
}

impl Locale {
    /// Both languages, in display order.
    pub const ALL: [Self; 2] = [Self::English, Self::Chinese];

    /// The string table for this language.
    pub fn strings(self) -> &'static Strings {
        match self {
            Self::English => &ENGLISH,
            Self::Chinese => &CHINESE,
        }
    }

    /// Name of the language in that language, for the toggle.
    pub fn endonym(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "中文",
        }
    }

    /// The other language.
    pub fn toggle(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|locale| *locale == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// One column of UI copy.
///
/// The struct is large but flat on purpose: a missing field is a compile error in
/// both languages at once, which is cheaper than a runtime fallback table.
#[derive(Debug)]
pub struct Strings {
    /// Name of the navigation group.
    pub workspace: &'static str,
    /// Navigation entry: local shells.
    pub nav_local: &'static str,
    /// One-line description of the local shells destination.
    pub nav_local_summary: &'static str,
    /// Navigation entry: saved hosts.
    pub nav_hosts: &'static str,
    /// One-line description of the hosts destination.
    pub nav_hosts_summary: &'static str,
    /// Navigation entry: open sessions.
    pub nav_sessions: &'static str,
    /// One-line description of the sessions destination.
    pub nav_sessions_summary: &'static str,
    /// Navigation entry: file transfers.
    pub nav_transfers: &'static str,
    /// One-line description of the transfers destination.
    pub nav_transfers_summary: &'static str,
    /// Empty-state title on the local shells destination.
    pub empty_local_title: &'static str,
    /// Empty-state body on the local shells destination.
    pub empty_local_body: &'static str,
    /// Empty-state title on the hosts destination.
    pub empty_hosts_title: &'static str,
    /// Empty-state body on the hosts destination.
    pub empty_hosts_body: &'static str,
    /// Empty-state title on the sessions destination.
    pub empty_sessions_title: &'static str,
    /// Empty-state body on the sessions destination.
    pub empty_sessions_body: &'static str,
    /// Empty-state title on the transfers destination.
    pub empty_transfers_title: &'static str,
    /// Empty-state body on the transfers destination.
    pub empty_transfers_body: &'static str,
    /// Chip label for a capability this build does not ship.
    pub state_planned: &'static str,
    /// Title of the planned panel.
    pub planned_title: &'static str,
    /// Body of the planned panel for local shells.
    pub planned_local: &'static str,
    /// Body of the planned panel for hosts.
    pub planned_hosts: &'static str,
    /// Body of the planned panel for sessions.
    pub planned_sessions: &'static str,
    /// Body of the planned panel for transfers.
    pub planned_transfers: &'static str,
    /// Chip label for an empty, loaded section.
    pub state_empty: &'static str,
    /// Chip label for a section that is loading.
    pub state_loading: &'static str,
    /// Title shown while a section is loading.
    pub loading_title: &'static str,
    /// Body shown while a section is loading.
    pub loading_body: &'static str,
    /// Chip label for a failed section.
    pub state_failed: &'static str,
    /// Title of a failed section.
    pub failed_title: &'static str,
    /// Label introducing the suggested recovery step.
    pub recovery_label: &'static str,
    /// Recovery action: retry.
    pub recovery_retry: &'static str,
    /// Recovery action: create a host.
    pub recovery_create_host: &'static str,
    /// Recovery action: import an SSH configuration.
    pub recovery_import_config: &'static str,
    /// Recovery action: pick a shell.
    pub recovery_choose_shell: &'static str,
    /// Label introducing the next step of an empty section.
    pub next_step: &'static str,
    /// Primary action on the local shells destination.
    pub primary_local: &'static str,
    /// Primary action on the hosts destination.
    pub primary_hosts: &'static str,
    /// Primary action on the sessions destination.
    pub primary_sessions: &'static str,
    /// Primary action on the transfers destination.
    pub primary_transfers: &'static str,
    /// Feedback after an unavailable action was requested.
    pub unavailable_notice: &'static str,
    /// Label above the list of rows.
    pub object_label: &'static str,
    /// Shortcut hint in the rail footer.
    pub hint_nav: &'static str,
    /// Label of the zoom control.
    pub zoom_control: &'static str,
    /// Label of the language toggle.
    pub language_control: &'static str,
    /// Suffix for a count of hosts.
    pub count_hosts: &'static str,
    /// Suffix for a count of sessions.
    pub count_sessions: &'static str,
    /// Suffix for a count of transfers.
    pub count_transfers: &'static str,
    /// Fallback name for a local session without a chosen shell.
    pub local_shell_fallback: &'static str,
    /// Hint shown when nothing is running at all.
    pub nothing_running: &'static str,
}

/// English copy.
pub static ENGLISH: Strings = Strings {
    workspace: "Workspace",
    nav_local: "Local shells",
    nav_local_summary: "A shell on this machine",
    nav_hosts: "Hosts",
    nav_hosts_summary: "Saved servers and SSH settings",
    nav_sessions: "Sessions",
    nav_sessions_summary: "Every open terminal",
    nav_transfers: "Transfers",
    nav_transfers_summary: "Uploads and downloads",
    empty_local_title: "No local shell is open",
    empty_local_body: "A local shell runs in the current directory of this machine and needs no network.",
    empty_hosts_title: "No saved hosts yet",
    empty_hosts_body: "A saved host keeps the address, port and user so a connection does not have to be retyped.",
    empty_sessions_title: "No session is open",
    empty_sessions_body: "Local and remote terminals appear here with their own name, type and state.",
    empty_transfers_title: "No transfer yet",
    empty_transfers_body: "Uploads and downloads list their source, target and progress here.",
    state_planned: "Not in this build",
    planned_title: "This part is not built yet",
    planned_local: "the local terminal that starts a real shell, shows output and accepts typing arrives with the terminal step.",
    planned_hosts: "saving, editing and importing hosts arrives with the host management step; nothing is stored or connected yet.",
    planned_sessions: "opening, switching and reconnecting sessions arrives with the session step; no terminal is running.",
    planned_transfers: "uploading and downloading over SFTP arrives with the transfer step; no file has been touched.",
    state_empty: "Empty",
    state_loading: "Loading",
    loading_title: "Waiting for real data",
    loading_body: "Nothing is shown here until the system reports what actually exists.",
    state_failed: "Failed",
    failed_title: "This could not be completed",
    recovery_label: "Suggested next step",
    recovery_retry: "Try again",
    recovery_create_host: "Create a host",
    recovery_import_config: "Import an SSH configuration",
    recovery_choose_shell: "Choose a shell",
    next_step: "Next step",
    primary_local: "New local shell",
    primary_hosts: "New host",
    primary_sessions: "Open session",
    primary_transfers: "Start a transfer",
    unavailable_notice: "This action is not available in this build, so nothing was started.",
    object_label: "Current object",
    hint_nav: "Ctrl+1-4 switches sections",
    zoom_control: "Text size",
    language_control: "Language",
    count_hosts: "host",
    count_sessions: "session",
    count_transfers: "transfer",
    local_shell_fallback: "System shell",
    nothing_running: "Nothing is running yet.",
};

/// Chinese copy.
pub static CHINESE: Strings = Strings {
    workspace: "工作区",
    nav_local: "本地终端",
    nav_local_summary: "在本机上运行的 Shell",
    nav_hosts: "主机",
    nav_hosts_summary: "已保存的服务器与 SSH 设置",
    nav_sessions: "会话",
    nav_sessions_summary: "所有已打开的终端",
    nav_transfers: "传输",
    nav_transfers_summary: "上传与下载",
    empty_local_title: "还没有打开本地终端",
    empty_local_body: "本地终端在当前机器的当前目录运行，不需要网络。",
    empty_hosts_title: "还没有保存的主机",
    empty_hosts_body: "保存主机后会记住地址、端口和用户名，下次连接不必重新输入。",
    empty_sessions_title: "还没有会话",
    empty_sessions_body: "本地与远程终端会各自显示名称、类型和当前状态。",
    empty_transfers_title: "还没有传输任务",
    empty_transfers_body: "上传与下载会在这里显示源、目标和进度。",
    state_planned: "当前版本未包含",
    planned_title: "这部分还没有实现",
    planned_local: "真正启动 Shell、显示输出并接受输入的本地终端会随终端步骤一起提供。",
    planned_hosts: "保存、编辑和导入主机随主机管理步骤一起提供；目前不会写入配置，也不会发起连接。",
    planned_sessions: "打开、切换和重连会话随会话步骤一起提供；目前没有任何终端在运行。",
    planned_transfers: "通过 SFTP 上传与下载随传输步骤一起提供；目前没有改动任何文件。",
    state_empty: "空",
    state_loading: "加载中",
    loading_title: "正在等待真实数据",
    loading_body: "在系统返回实际结果之前，这里不会显示任何内容。",
    state_failed: "失败",
    failed_title: "这一步没能完成",
    recovery_label: "建议的下一步",
    recovery_retry: "重试",
    recovery_create_host: "新建主机",
    recovery_import_config: "导入 SSH 配置",
    recovery_choose_shell: "选择 Shell",
    next_step: "下一步",
    primary_local: "新建本地终端",
    primary_hosts: "新建主机",
    primary_sessions: "打开会话",
    primary_transfers: "开始传输",
    unavailable_notice: "当前版本没有实现这个操作，因此没有启动任何任务。",
    object_label: "当前对象",
    hint_nav: "Ctrl+1-4 切换分区",
    zoom_control: "文字大小",
    language_control: "语言",
    count_hosts: "台主机",
    count_sessions: "个会话",
    count_transfers: "个传输",
    local_shell_fallback: "系统 Shell",
    nothing_running: "暂无正在运行的任务。",
};

/// Localized label for a session lifecycle state.
///
/// English reuses the domain label so the two cannot drift; Chinese is written
/// here because the domain crate stays language-neutral.
pub fn session_state_label(state: &SessionState, locale: Locale) -> String {
    match locale {
        Locale::English => state.label().to_owned(),
        Locale::Chinese => match state {
            SessionState::Idle => "未开始",
            SessionState::Connecting => "连接中",
            SessionState::VerifyingHost => "校验主机身份",
            SessionState::Authenticating => "认证中",
            SessionState::Connected => "已连接",
            SessionState::Disconnected(_) => "已断开",
            SessionState::Failed(_) => "失败",
            SessionState::Closed => "已关闭",
        }
        .to_owned(),
    }
}

/// Localized label for a transfer state.
pub fn transfer_state_label(state: &TransferState, locale: Locale) -> String {
    match state {
        TransferState::Queued => match locale {
            Locale::English => "Queued".to_owned(),
            Locale::Chinese => "排队中".to_owned(),
        },
        TransferState::Running { .. } => match locale {
            Locale::English => "Running".to_owned(),
            Locale::Chinese => "进行中".to_owned(),
        },
        TransferState::Completed { .. } => match locale {
            Locale::English => "Completed".to_owned(),
            Locale::Chinese => "已完成".to_owned(),
        },
        TransferState::Failed(_) => match locale {
            Locale::English => "Failed".to_owned(),
            Locale::Chinese => "失败".to_owned(),
        },
        TransferState::Cancelled => match locale {
            Locale::English => "Cancelled".to_owned(),
            Locale::Chinese => "已取消".to_owned(),
        },
    }
}
