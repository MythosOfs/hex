use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn expand(before: &str) -> Option<(&str, &str, String)> {
    let start = before
        .rfind(char::is_whitespace)
        .map_or(0, |i| i + before[i..].chars().next().unwrap().len_utf8());
    let (prefix, key) = before.split_at(start);
    let value = match key {
        ";datum" | ";zeit" => {
            let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
            let seconds = libc::time_t::try_from(seconds).ok()?;
            // SAFETY: localtime_r writes to the provided tm and returns null on failure.
            let now = unsafe {
                let mut local = std::mem::zeroed::<libc::tm>();
                (!libc::localtime_r(&seconds, &mut local).is_null()).then_some(local)?
            };
            if key == ";datum" {
                format!(
                    "{:02}.{:02}.{}",
                    now.tm_mday,
                    now.tm_mon + 1,
                    now.tm_year + 1900
                )
            } else {
                format!("{:02}:{:02}", now.tm_hour, now.tm_min)
            }
        }
        ";lg" => "Liebe Grüße".into(),
        ";pfeil" => "→".into(),
        ";check" => "✓".into(),
        _ => return None,
    };
    Some((prefix, key, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_require_a_standalone_exact_trigger() {
        assert_eq!(
            expand("Ein Test\u{a0};check"),
            Some(("Ein Test\u{a0}", ";check", "✓".into()))
        );
        assert!(expand("foo;check").is_none());
        assert!(expand(";checked").is_none());
    }
}
