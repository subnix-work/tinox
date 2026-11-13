//! `tinox guide [topic]`: print the compact language guide embedded in the compiler.
//!
//! The text is `docs/AI_GUIDE.md` (generated and checked by `make docs-check`), so an agent can
//! pull the guide matching *this* compiler without knowing where the repository lives. Topic
//! pages are added to `TOPICS` as they are written.

/// (name, one-line description, content)
const TOPICS: &[(&str, &str, &str)] = &[(
    "core",
    "language rules, entry point, annotations, tooling",
    include_str!("../../../docs/AI_GUIDE.md"),
)];

pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None => print_topic("core"),
        Some("--list") | Some("list") => {
            for (name, about, _) in TOPICS {
                println!("{name:<12} {about}");
            }
            Ok(())
        }
        Some(topic) if !topic.starts_with('-') => print_topic(topic),
        Some(flag) => Err(format!("unknown option {flag}; usage: tinox guide [topic | --list]")),
    }
}

fn print_topic(name: &str) -> Result<(), String> {
    match TOPICS.iter().find(|(n, _, _)| *n == name) {
        Some((_, _, content)) => {
            print!("{content}");
            Ok(())
        }
        None => {
            let known: Vec<&str> = TOPICS.iter().map(|(n, _, _)| *n).collect();
            Err(format!("unknown guide topic '{name}'; available: {}", known.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_topic_is_the_embedded_guide() {
        assert!(TOPICS[0].2.starts_with("# Tinox compact guide"));
    }

    #[test]
    fn unknown_topic_is_a_hard_error_listing_the_topics() {
        let err = run(&["nope".to_string()]).unwrap_err();
        assert!(err.contains("unknown guide topic 'nope'") && err.contains("core"), "{err}");
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(run(&["--bogus".to_string()]).is_err());
    }
}
