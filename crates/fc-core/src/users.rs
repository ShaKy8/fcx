//! uid/gid → name lookups from `/etc/passwd` and `/etc/group`, cached for the
//! process lifetime (the properties dialog and permission editors need them).

use std::collections::HashMap;
use std::sync::OnceLock;

static USERS: OnceLock<HashMap<u32, String>> = OnceLock::new();
static GROUPS: OnceLock<HashMap<u32, String>> = OnceLock::new();

/// `name:x:id:…` lines → id → name. Malformed lines are skipped.
fn parse_ids(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _password = fields.next()?;
            let id = fields.next()?.parse().ok()?;
            Some((id, name.to_owned()))
        })
        .collect()
}

fn load(path: &str) -> HashMap<u32, String> {
    std::fs::read_to_string(path)
        .map(|t| parse_ids(&t))
        .unwrap_or_default()
}

/// Login name for `uid`, or the number as text if unknown.
pub fn user_name(uid: u32) -> String {
    USERS
        .get_or_init(|| load("/etc/passwd"))
        .get(&uid)
        .cloned()
        .unwrap_or_else(|| uid.to_string())
}

/// Group name for `gid`, or the number as text if unknown.
pub fn group_name(gid: u32) -> String {
    GROUPS
        .get_or_init(|| load("/etc/group"))
        .get(&gid)
        .cloned()
        .unwrap_or_else(|| gid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_passwd_style_lines() {
        let table = parse_ids(
            "root:x:0:0:root:/root:/bin/bash\n\
             kyle:x:1000:1000::/home/kyle:/bin/zsh\n\
             broken line\n\
             nobody:x:notanumber:65534::/:/sbin/nologin\n",
        );
        assert_eq!(table.get(&0).map(String::as_str), Some("root"));
        assert_eq!(table.get(&1000).map(String::as_str), Some("kyle"));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn root_resolves_and_unknown_ids_fall_back_to_numbers() {
        assert_eq!(user_name(0), "root");
        assert_eq!(group_name(0), "root");
        assert_eq!(user_name(4_000_000_000), "4000000000");
    }
}
