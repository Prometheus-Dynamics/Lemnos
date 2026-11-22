//! The restart socket's protocol: one request line from the caller, answer lines from the helper.
//!
//! A request is `<verb> <nonce>` and a newline, nothing else: the verb is `check` or `reboot`, the
//! nonce one to 32 lower-case hex digits that the caller picks and every answer repeats. The helper
//! takes no other input: no unit name, no argument, no option.
//!
//! Answers, each one line:
//! - `ok <nonce>`: `check` passed.
//! - `refused <nonce> <reason>`: the request was not carried out (`-` for the nonce when the request
//!   could not be read).
//! - `accepted <nonce>`: `reboot` is about to start the reboot. One of these follows on the same
//!   connection:
//!   - `stopping <nonce>`: the reboot started and the system is stopping.
//!   - `failed <nonce> <reason>`: the reboot did not start, or the system did not start stopping.

/// The longest request line, newline included.
pub const MAX_REQUEST: usize = 48;
/// The longest nonce.
pub const MAX_NONCE: usize = 32;

/// What a caller may ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Whether a reboot would be accepted: everything a reboot does except starting it.
    Check,
    /// Reboot the board.
    Reboot,
}

impl Verb {
    /// The verb's word on the wire.
    pub fn word(self) -> &'static str {
        match self {
            Verb::Check => "check",
            Verb::Reboot => "reboot",
        }
    }
}

/// A request: its verb and nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub verb: Verb,
    pub nonce: String,
}

/// Whether `nonce` is one the protocol carries.
pub fn valid_nonce(nonce: &str) -> bool {
    !nonce.is_empty()
        && nonce.len() <= MAX_NONCE
        && nonce
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reads a request line (without its newline). Anything but exactly `<verb> <nonce>` is refused,
/// with the reason.
pub fn parse_request(line: &str) -> Result<Request, String> {
    let mut words = line.split(' ');
    let (Some(verb), Some(nonce), None) = (words.next(), words.next(), words.next()) else {
        return Err("a request is exactly \"<verb> <nonce>\"".to_string());
    };
    let verb = match verb {
        "check" => Verb::Check,
        "reboot" => Verb::Reboot,
        _ => return Err("the verb is check or reboot".to_string()),
    };
    if !valid_nonce(nonce) {
        return Err(format!(
            "the nonce is 1 to {MAX_NONCE} lower-case hex digits"
        ));
    }
    Ok(Request {
        verb,
        nonce: nonce.to_string(),
    })
}

/// An answer line, as read by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Ok,
    Refused(String),
    Accepted,
    Stopping,
    Failed(String),
}

/// The line for `answer` to the request with `nonce`, newline included. A reason is kept to one
/// line.
pub fn answer_line(nonce: &str, answer: &Answer) -> String {
    let one_line = |s: &str| s.replace(['\n', '\r'], " ");
    match answer {
        Answer::Ok => format!("ok {nonce}\n"),
        Answer::Refused(why) => format!("refused {nonce} {}\n", one_line(why)),
        Answer::Accepted => format!("accepted {nonce}\n"),
        Answer::Stopping => format!("stopping {nonce}\n"),
        Answer::Failed(why) => format!("failed {nonce} {}\n", one_line(why)),
    }
}

/// Reads an answer line (without its newline) to the request with `nonce`. A line for another
/// nonce, or one this protocol does not have, is an error naming it.
pub fn parse_answer(line: &str, nonce: &str) -> Result<Answer, String> {
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    let (their, reason) = rest.split_once(' ').unwrap_or((rest, ""));
    if their != nonce && !(word == "refused" && their == "-") {
        return Err(format!(
            "the restart helper answered {line:?}, not to this request ({nonce})"
        ));
    }
    let bare = |a: Answer| {
        if reason.is_empty() {
            Ok(a)
        } else {
            Err(format!("the restart helper answered {line:?}"))
        }
    };
    match word {
        "ok" => bare(Answer::Ok),
        "accepted" => bare(Answer::Accepted),
        "stopping" => bare(Answer::Stopping),
        "refused" => Ok(Answer::Refused(reason.to_string())),
        "failed" => Ok(Answer::Failed(reason.to_string())),
        _ => Err(format!("the restart helper answered {line:?}")),
    }
}
