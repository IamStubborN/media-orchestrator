use super::NotificationValidationError;

const MAX_NOTIFICATION_DISPLAY_BYTES: usize = 256;
const MAX_NOTIFICATION_CARD_KEY_BYTES: usize = 96;

pub(super) fn validate_display_field(value: &str) -> Result<(), NotificationValidationError> {
    if value.trim().is_empty()
        || value.len() > MAX_NOTIFICATION_DISPLAY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(NotificationValidationError::InvalidDisplayField);
    }
    Ok(())
}

pub(super) fn validate_human_label(value: &str) -> Result<(), NotificationValidationError> {
    validate_public_display_field(value)?;
    if !value.chars().all(|character| {
        character.is_alphanumeric()
            || character.is_whitespace()
            || matches!(
                character,
                '.' | ','
                    | ':'
                    | '!'
                    | '?'
                    | '\''
                    | '"'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '+'
                    | '-'
                    | '_'
                    | '/'
            )
    }) {
        return Err(NotificationValidationError::InvalidDisplayField);
    }
    Ok(())
}

pub(super) fn validate_codec(value: &str) -> Result<(), NotificationValidationError> {
    validate_machine_metadata(value, |character| {
        character.is_ascii_alphanumeric()
            || matches!(character, ' ' | '.' | '_' | '-' | '/' | '+' | '@' | ':')
    })
}

pub(super) fn validate_profile(value: &str) -> Result<(), NotificationValidationError> {
    validate_machine_metadata(value, |character| {
        character.is_ascii_alphanumeric()
            || matches!(character, ' ' | '.' | '_' | '-' | '+' | '@' | ':')
    })
}

pub(super) fn validate_language(value: &str) -> Result<(), NotificationValidationError> {
    validate_machine_metadata(value, |character| {
        character.is_alphanumeric()
            || character.is_whitespace()
            || matches!(character, '-' | '_' | '/' | '(' | ')')
    })
}

pub(super) fn validate_channel_layout(value: &str) -> Result<(), NotificationValidationError> {
    validate_machine_metadata(value, |character| {
        character.is_ascii_alphanumeric()
            || matches!(character, ' ' | '.' | '_' | '-' | '+' | '(' | ')')
    })
}

pub(super) fn validate_machine_metadata(
    value: &str,
    is_allowed: impl Fn(char) -> bool,
) -> Result<(), NotificationValidationError> {
    validate_public_display_field(value)?;
    if !value.chars().all(is_allowed) {
        return Err(NotificationValidationError::InvalidDisplayField);
    }
    Ok(())
}

fn validate_public_display_field(value: &str) -> Result<(), NotificationValidationError> {
    validate_display_field(value)?;

    let trimmed = value.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    if lowercase.contains("://")
        || lowercase.starts_with("www.")
        || lowercase.starts_with("magnet:?")
        || is_path_like(trimmed)
        || is_shell_command_like(&lowercase)
        || contains_secret_label(&lowercase)
        || is_internal_error_code(trimmed)
    {
        return Err(NotificationValidationError::InvalidDisplayField);
    }
    Ok(())
}

fn is_path_like(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.contains('\\')
        || (value.starts_with('~')
            && value[1..]
                .chars()
                .take_while(|character| !character.is_whitespace())
                .any(|character| matches!(character, '/' | '\\')))
        || value.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
        || (value.contains('/') && has_path_extension(value))
}

fn has_path_extension(value: &str) -> bool {
    const PATH_EXTENSIONS: [&str; 26] = [
        ".mkv", ".mp4", ".m4v", ".avi", ".mov", ".mpg", ".mpeg", ".webm", ".ts", ".m2ts", ".srt",
        ".ass", ".ssa", ".vtt", ".sub", ".idx", ".conf", ".config", ".ini", ".yaml", ".yml",
        ".json", ".toml", ".sh", ".ps1", ".bat",
    ];
    let lowercase = value.to_ascii_lowercase();
    PATH_EXTENSIONS
        .iter()
        .any(|extension| lowercase.ends_with(extension))
}

fn is_shell_command_like(value: &str) -> bool {
    const COMMANDS: [&str; 23] = [
        "curl",
        "wget",
        "bash",
        "sh",
        "zsh",
        "pwsh",
        "powershell",
        "cmd",
        "sudo",
        "rm",
        "python",
        "python3",
        "ffmpeg",
        "yt-dlp",
        "ls",
        "cat",
        "find",
        "head",
        "tail",
        "less",
        "more",
        "env",
        "printenv",
    ];

    value.starts_with('-')
        || value.contains("$(")
        || value.contains('`')
        || value
            .chars()
            .any(|character| matches!(character, ';' | '|' | '&' | '$' | '<' | '>'))
        || COMMANDS.iter().any(|command| {
            value == *command
                || value
                    .strip_prefix(command)
                    .and_then(|suffix| suffix.chars().next())
                    .is_some_and(char::is_whitespace)
        })
}

fn contains_secret_label(value: &str) -> bool {
    const LABELS: [&str; 14] = [
        "api key",
        "api_key",
        "api-key",
        "access token",
        "access_token",
        "authorization",
        "token",
        "password",
        "passwd",
        "secret",
        "credential",
        "private key",
        "private_key",
        "private-key",
    ];

    LABELS.iter().any(|label| {
        value.match_indices(label).any(|(index, _)| {
            let prefix_is_boundary = index == 0
                || value.as_bytes()[index - 1].is_ascii_whitespace()
                || matches!(value.as_bytes()[index - 1], b';' | b',');
            let suffix = &value[index + label.len()..];
            prefix_is_boundary
                && matches!(suffix.trim_start().as_bytes().first(), Some(b':' | b'='))
        })
    }) || value.match_indices("bearer").any(|(index, _)| {
        let prefix_is_boundary = index == 0
            || value.as_bytes()[index - 1].is_ascii_whitespace()
            || matches!(value.as_bytes()[index - 1], b';' | b',');
        prefix_is_boundary
            && value[index + "bearer".len()..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
    })
}

fn is_internal_error_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    let error_number = bytes.len() >= 4
        && matches!(bytes[0], b'E' | b'e')
        && bytes[1..].iter().all(|byte| byte.is_ascii_digit());
    let lower = value.to_ascii_lowercase();
    error_number
        || (value.contains('_')
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            && lower.split('_').any(|segment| {
                matches!(
                    segment,
                    "error"
                        | "failed"
                        | "failure"
                        | "invalid"
                        | "not"
                        | "found"
                        | "unavailable"
                        | "forbidden"
                        | "denied"
                        | "timeout"
                        | "internal"
                        | "exception"
                )
            }))
}

pub(super) fn valid_card_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NOTIFICATION_CARD_KEY_BYTES
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, ':' | '-'))
}
