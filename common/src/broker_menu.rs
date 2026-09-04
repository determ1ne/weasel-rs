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
pub const ITEMS: &[(u32, &str)] = &[
    (USER_DIRECTORY, "用户文件夹 (&U)"),
    (PROGRAM_DIRECTORY, "程序文件夹 (&P)"),
    (LOG_DIRECTORY, "日志文件夹 (&L)"),
    (0, ""),
    (HELP, "帮助 (&H)"),
    (FORUM, "论坛 (&F)"),
    (0, ""),
    (DEPLOY, "重新部署Rime (&D)"),
    (RESTART, "重启 (&R)"),
    (EXIT, "退出 (&X)"),
];

pub fn is_command(id: u32) -> bool {
    id != 0 && ITEMS.iter().any(|(command, _)| *command == id)
}

#[cfg(test)]
mod tests {
    use super::*;
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
