use once_cell::sync::Lazy;
use std::collections::HashMap;

static SLACK_EMOJI: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("+1", "\u{1F44D}");
    m.insert("thumbsup", "\u{1F44D}");
    m.insert("-1", "\u{1F44E}");
    m.insert("thumbsdown", "\u{1F44E}");
    m.insert("heart", "\u{2764}\u{FE0F}");
    m.insert("heart_eyes", "\u{1F60D}");
    m.insert("joy", "\u{1F602}");
    m.insert("rofl", "\u{1F923}");
    m.insert("smile", "\u{1F604}");
    m.insert("grinning", "\u{1F600}");
    m.insert("smiley", "\u{1F603}");
    m.insert("laughing", "\u{1F606}");
    m.insert("wink", "\u{1F609}");
    m.insert("blush", "\u{1F60A}");
    m.insert("yum", "\u{1F60B}");
    m.insert("sunglasses", "\u{1F60E}");
    m.insert("thinking_face", "\u{1F914}");
    m.insert("thinking", "\u{1F914}");
    m.insert("raised_hands", "\u{1F64C}");
    m.insert("clap", "\u{1F44F}");
    m.insert("fire", "\u{1F525}");
    m.insert("100", "\u{1F4AF}");
    m.insert("tada", "\u{1F389}");
    m.insert("party_popper", "\u{1F389}");
    m.insert("rocket", "\u{1F680}");
    m.insert("star", "\u{2B50}");
    m.insert("eyes", "\u{1F440}");
    m.insert("wave", "\u{1F44B}");
    m.insert("pray", "\u{1F64F}");
    m.insert("muscle", "\u{1F4AA}");
    m.insert("ok_hand", "\u{1F44C}");
    m.insert("v", "\u{270C}\u{FE0F}");
    m.insert("point_up", "\u{261D}\u{FE0F}");
    m.insert("point_down", "\u{1F447}");
    m.insert("point_left", "\u{1F448}");
    m.insert("point_right", "\u{1F449}");
    m.insert("sob", "\u{1F62D}");
    m.insert("cry", "\u{1F622}");
    m.insert("angry", "\u{1F620}");
    m.insert("rage", "\u{1F621}");
    m.insert("scream", "\u{1F631}");
    m.insert("fearful", "\u{1F628}");
    m.insert("sweat", "\u{1F613}");
    m.insert("disappointed", "\u{1F61E}");
    m.insert("confused", "\u{1F615}");
    m.insert("neutral_face", "\u{1F610}");
    m.insert("expressionless", "\u{1F611}");
    m.insert("unamused", "\u{1F612}");
    m.insert("rolling_eyes", "\u{1F644}");
    m.insert("grimacing", "\u{1F62C}");
    m.insert("relieved", "\u{1F60C}");
    m.insert("pensive", "\u{1F614}");
    m.insert("sleepy", "\u{1F62A}");
    m.insert("sleeping", "\u{1F634}");
    m.insert("mask", "\u{1F637}");
    m.insert("nerd_face", "\u{1F913}");
    m.insert("worried", "\u{1F61F}");
    m.insert("flushed", "\u{1F633}");
    m.insert("hugs", "\u{1F917}");
    m.insert("hugging_face", "\u{1F917}");
    m.insert("cowboy_hat_face", "\u{1F920}");
    m.insert("clown_face", "\u{1F921}");
    m.insert("shushing_face", "\u{1F92B}");
    m.insert("exploding_head", "\u{1F92F}");
    m.insert("partying_face", "\u{1F973}");
    m.insert("star_struck", "\u{1F929}");
    m.insert("money_mouth_face", "\u{1F911}");
    m.insert("zany_face", "\u{1F92A}");
    m.insert("skull", "\u{1F480}");
    m.insert("ghost", "\u{1F47B}");
    m.insert("alien", "\u{1F47D}");
    m.insert("robot_face", "\u{1F916}");
    m.insert("poop", "\u{1F4A9}");
    m.insert("hankey", "\u{1F4A9}");
    m.insert("see_no_evil", "\u{1F648}");
    m.insert("hear_no_evil", "\u{1F649}");
    m.insert("speak_no_evil", "\u{1F64A}");
    m.insert("kiss", "\u{1F48B}");
    m.insert("cupid", "\u{1F498}");
    m.insert("sparkling_heart", "\u{1F496}");
    m.insert("broken_heart", "\u{1F494}");
    m.insert("orange_heart", "\u{1F9E1}");
    m.insert("yellow_heart", "\u{1F49B}");
    m.insert("green_heart", "\u{1F49A}");
    m.insert("blue_heart", "\u{1F499}");
    m.insert("purple_heart", "\u{1F49C}");
    m.insert("black_heart", "\u{1F5A4}");
    m.insert("white_heart", "\u{1F90D}");
    m.insert("two_hearts", "\u{1F495}");
    m.insert("revolving_hearts", "\u{1F49E}");
    m.insert("check", "\u{2705}");
    m.insert("white_check_mark", "\u{2705}");
    m.insert("x", "\u{274C}");
    m.insert("heavy_check_mark", "\u{2714}\u{FE0F}");
    m.insert("warning", "\u{26A0}\u{FE0F}");
    m.insert("no_entry", "\u{26D4}");
    m.insert("question", "\u{2753}");
    m.insert("exclamation", "\u{2757}");
    m
});

/// Convert a Slack emoji name to its Unicode character.
pub fn slack_emoji_to_unicode(name: &str) -> String {
    // Handle skin tone modifiers
    let base_name = if let Some(idx) = name.find("::skin-tone-") {
        &name[..idx]
    } else {
        name
    };

    if let Some(&emoji) = SLACK_EMOJI.get(base_name) {
        emoji.to_string()
    } else {
        format!(":{}:", name)
    }
}

/// Replace :emoji_name: patterns in text with Unicode characters.
pub fn convert_slack_emojis(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if c == ':' {
            // Look for closing :
            let rest = &text[i + 1..];
            if let Some(end) = rest.find(':') {
                let name = &rest[..end];
                // Emoji names are alphanumeric with underscores, hyphens, plus, minus
                if !name.is_empty()
                    && !name.contains(' ')
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '+')
                {
                    let converted = slack_emoji_to_unicode(name);
                    if !converted.starts_with(':') {
                        result.push_str(&converted);
                        // Skip past the closing colon
                        for _ in 0..=end {
                            chars.next();
                        }
                        continue;
                    }
                }
            }
            result.push(c);
        } else {
            result.push(c);
        }
    }

    result
}

/// Convert Slack user mentions <@U12345> to @name.
pub fn convert_slack_mentions(text: &str, resolve_user: &impl Fn(&str) -> String) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("<@") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('>') {
            let inner = &after[..end];
            // Could be <@U12345> or <@U12345|name>
            let user_id = if let Some(pipe) = inner.find('|') {
                &inner[..pipe]
            } else {
                inner
            };
            let name = resolve_user(user_id);
            result.push('@');
            result.push_str(&name);
            rest = &after[end + 1..];
        } else {
            result.push_str("<@");
            rest = after;
        }
    }
    result.push_str(rest);
    result
}

/// Convert Slack link format <URL|text> and <URL> to just the URL.
pub fn convert_slack_links(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('<') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('>') {
            let inner = &after[..end];
            if inner.starts_with("http://") || inner.starts_with("https://") {
                // <URL|text> -> URL, <URL> -> URL
                let url = if let Some(pipe) = inner.find('|') {
                    &inner[..pipe]
                } else {
                    inner
                };
                result.push_str(url);
            } else if inner.starts_with('@') {
                // User mention - keep as-is with angle brackets for convert_slack_mentions
                result.push('<');
                result.push_str(inner);
                result.push('>');
            } else {
                result.push_str(inner);
            }
            rest = &after[end + 1..];
        } else {
            result.push('<');
            rest = after;
        }
    }
    result.push_str(rest);
    result
}

/// Remove skin-tone modifiers like :skin-tone-6: from text
fn remove_skin_tone_modifiers(text: &str) -> String {
    // Pattern: :skin-tone-X: where X is a digit
    // Use a simple string replacement since we know the exact pattern
    let mut result = text.to_string();
    for i in 1..=6 {
        let pattern = format!(":skin-tone-{}:", i);
        result = result.replace(&pattern, "");
    }
    result
}

/// Format message text: convert links, mentions, and emojis.
pub fn format_message_text(
    text: &str,
    show_emojis: bool,
    resolve_user: &impl Fn(&str) -> String,
) -> String {
    let mut out = convert_slack_links(text);
    out = remove_skin_tone_modifiers(&out);
    out = convert_slack_mentions(&out, resolve_user);
    if show_emojis {
        out = convert_slack_emojis(&out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emoji_conversion() {
        assert_eq!(
            convert_slack_emojis("hello :fire: world"),
            "hello \u{1F525} world"
        );
        assert_eq!(convert_slack_emojis(":thumbsup:"), "\u{1F44D}");
        assert_eq!(convert_slack_emojis(":unknown_emoji:"), ":unknown_emoji:");
        assert_eq!(convert_slack_emojis("no emojis here"), "no emojis here");
    }

    #[test]
    fn test_slack_links() {
        assert_eq!(
            convert_slack_links("<https://example.com|click>"),
            "https://example.com"
        );
        assert_eq!(
            convert_slack_links("<https://example.com>"),
            "https://example.com"
        );
    }

    #[test]
    fn test_mentions() {
        let resolve = |id: &str| -> String {
            if id == "U123" {
                "Alice".into()
            } else {
                id.into()
            }
        };
        assert_eq!(convert_slack_mentions("hi <@U123>", &resolve), "hi @Alice");
        assert_eq!(
            convert_slack_mentions("hi <@U123|bob>", &resolve),
            "hi @Alice"
        );
    }
}
