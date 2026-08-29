pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

pub fn truncate_chars_with_ellipsis(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out = truncate_chars(input, max_chars);
    out.push_str("...");
    out
}

pub fn escape_html(input: &str) -> String {
    html_escape::encode_text(input).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_unicode_safe() {
        assert_eq!(truncate_chars("ab█😊世界", 4), "ab█😊");
        assert_eq!(truncate_chars_with_ellipsis("ab█😊世界", 4), "ab█😊...");
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(
            escape_html("<b>&\"x\"</b>"),
            "&lt;b&gt;&amp;\"x\"&lt;/b&gt;"
        );
    }
}
