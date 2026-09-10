//! Shared broker command IDs and menu order. Commands carry no pointers.
pub const WINDOW_CLASS: &str = "weasel-rs-broker";
pub const DEPLOY: u32 = 1001;
pub const EXIT: u32 = 1002;
pub const USER_DIRECTORY: u32 = 1003;
pub const PROGRAM_DIRECTORY: u32 = 1004;
pub const LOG_DIRECTORY: u32 = 1005;
pub const HELP: u32 = 1006;
pub const FORUM: u32 = 1007;
pub const RESTART: u32 = 1008;
pub const ABOUT: u32 = 1009;
pub const DIAGNOSTICS: u32 = 1010;
pub const SETTINGS: u32 = 1011;
pub const ITEMS: &[(u32, &str)] = &[
    (SETTINGS, "设置 (&S)"),
    (USER_DIRECTORY, "用户文件夹 (&U)"),
    (PROGRAM_DIRECTORY, "程序文件夹 (&P)"),
    (LOG_DIRECTORY, "日志文件夹 (&L)"),
    (0, ""),
    (HELP, "帮助 (&H)"),
    (FORUM, "论坛 (&F)"),
    (ABOUT, "关于小狼毫RS (&A)"),
    (0, ""),
    (DEPLOY, "重新部署Rime (&D)"),
    (RESTART, "重启 (&R)"),
    (EXIT, "退出 (&X)"),
];

pub fn items(diagnostics: bool) -> impl Iterator<Item = (u32, &'static str)> {
    ITEMS.iter().copied().flat_map(move |item| {
        [
            Some(item),
            (diagnostics && item.0 == ABOUT).then_some((DIAGNOSTICS, "诊断信息… (&I)")),
        ]
        .into_iter()
        .flatten()
    })
}

pub fn is_command(id: u32) -> bool {
    id != 0 && ITEMS.iter().any(|(command, _)| *command == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_is_only_in_shift_menu_and_about_is_always_visible() {
        for shift in [false, true] {
            let menu: Vec<_> = items(shift).collect();
            assert!(menu.iter().any(|(id, _)| *id == ABOUT));
            assert_eq!(menu.iter().any(|(id, _)| *id == DIAGNOSTICS), shift);
            let mut keys = std::collections::HashSet::new();
            for (id, label) in menu {
                if id != 0 {
                    assert!(keys.insert(label.split_once('&').unwrap().1.chars().next().unwrap()));
                }
            }
        }
    }
    #[test]
    fn menu_accelerators_are_unique() {
        let mut keys = std::collections::HashSet::new();
        for &(id, label) in ITEMS {
            if id == 0 {
                continue;
            }
            assert_eq!(label.matches('&').count(), 1);
            let key = label.split_once('&').unwrap().1.chars().next().unwrap();
            assert!(key.is_ascii_uppercase() && keys.insert(key));
        }
    }
    #[test]
    fn command_ids_are_unique_and_separators_are_not_commands() {
        let mut ids: Vec<_> = ITEMS
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| *id != 0)
            .collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
        assert!(!is_command(0));
        assert!(!is_command(u32::MAX));
        assert!(is_command(DEPLOY));
        assert!(is_command(EXIT));
    }
}
