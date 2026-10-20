use crate::model::{Action, ResultItem, score_text};
use std::io::Write;
use std::process::{Command, Stdio};

const MAX_PREVIEW_CHARS: usize = 160;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardEntry {
    /// The raw `cliphist list` line; `cliphist decode` reads it back verbatim.
    pub line: String,
    pub preview: String,
    pub is_image: bool,
}

pub(crate) fn load_clipboard_history(limit: usize) -> Vec<ClipboardEntry> {
    Command::new("cliphist")
        .arg("list")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_cliphist_list(&String::from_utf8_lossy(&output.stdout), limit))
        .unwrap_or_default()
}

pub(crate) fn parse_cliphist_list(text: &str, limit: usize) -> Vec<ClipboardEntry> {
    text.lines()
        .filter_map(|line| {
            let (_, preview) = line.split_once('\t')?;
            let preview = preview.split_whitespace().collect::<Vec<_>>().join(" ");
            if preview.is_empty() {
                return None;
            }
            Some(ClipboardEntry {
                line: line.to_string(),
                is_image: preview.starts_with("[[ binary data"),
                preview,
            })
        })
        .take(limit)
        .collect()
}

pub(crate) fn clipboard_match_score(entry: &ClipboardEntry, query: &str) -> Option<i32> {
    score_text(&entry.preview, query)
}

pub(crate) fn clipboard_result_item(entry: ClipboardEntry, score: i32) -> ResultItem {
    let title = if entry.preview.chars().count() > MAX_PREVIEW_CHARS {
        let truncated = entry
            .preview
            .chars()
            .take(MAX_PREVIEW_CHARS)
            .collect::<String>();
        format!("{truncated}…")
    } else {
        entry.preview
    };
    ResultItem {
        prediction_key: None,
        title,
        subtitle: if entry.is_image {
            "Copy this image again".to_string()
        } else {
            "Copy this text again".to_string()
        },
        source: "Clipboard",
        icon_name: if entry.is_image {
            "image-x-generic-symbolic"
        } else {
            "edit-paste-symbolic"
        }
        .to_string(),
        score,
        action: Action::RestoreClipboard { entry: entry.line },
        ..Default::default()
    }
}

/// Decodes a history entry and hands it to `wl-copy`, which keeps serving it
/// after Luma exits. The GTK clipboard would lose it when the window closes.
pub fn restore_clipboard_entry(line: &str) -> std::io::Result<()> {
    let mut decode = Command::new("cliphist")
        .arg("decode")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let decoded = decode.stdout.take().expect("cliphist stdout is piped");
    let mut copy = Command::new("wl-copy").stdin(decoded).spawn()?;
    decode
        .stdin
        .take()
        .expect("cliphist stdin is piped")
        .write_all(line.as_bytes())?;
    let decoded_ok = decode.wait()?.success();
    let copied_ok = copy.wait()?.success();
    if decoded_ok && copied_ok {
        Ok(())
    } else {
        Err(std::io::Error::other("cliphist decode or wl-copy failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::{ClipboardEntry, parse_cliphist_list};

    #[test]
    fn parses_text_and_image_entries() {
        let list = "42\thello   world\n41\t[[ binary data 12 KiB png 100x100 ]]\n40\t\n";

        assert_eq!(
            parse_cliphist_list(list, 10),
            vec![
                ClipboardEntry {
                    line: "42\thello   world".to_string(),
                    preview: "hello world".to_string(),
                    is_image: false,
                },
                ClipboardEntry {
                    line: "41\t[[ binary data 12 KiB png 100x100 ]]".to_string(),
                    preview: "[[ binary data 12 KiB png 100x100 ]]".to_string(),
                    is_image: true,
                },
            ]
        );
    }

    #[test]
    fn respects_the_limit() {
        let list = "3\tc\n2\tb\n1\ta\n";
        assert_eq!(parse_cliphist_list(list, 2).len(), 2);
    }
}
