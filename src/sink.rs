//! Where a dictation lands: the local Herdr server, or one reached over SSH.
//!
//! The surface is the plugin's intent, not Herdr's RPCs. One `deliver` carries
//! "these words, submitted or not", and each transport does what suits it: the
//! local arm two socket calls, the remote arm a single ssh invocation that
//! verifies and delivers together. Exposing `send_text` and `send_keys` here
//! would force the remote arm into the round trips it exists to avoid.

use std::time::Duration;

use crate::machine::{self, Machine};
use crate::remote::{self, Occupant, Target};
use crate::settings;
use crate::{Error, Result, ipc};

/// Refresh well inside the TTL so one lost call does not blank the label.
const LOCAL_TTL: Duration = Duration::from_secs(3);
const LOCAL_REFRESH: Duration = Duration::from_secs(1);
const REMOTE_TTL: Duration = Duration::from_secs(12);
const REMOTE_REFRESH: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub enum Sink {
    Local(ipc::Client),
    Remote(Box<Target>),
}

/// What delivery did, for the log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Typed,
    Submitted,
    /// Typed but deliberately not submitted: the agent had gone and the pane
    /// was back at a shell prompt.
    AgentGone,
}

/// The machine and pane a dictation was latched to.
#[derive(Clone, Debug)]
pub struct Latch {
    pub sink: Sink,
    pub pane: String,
    pub occupant: Occupant,
    pub machine: Option<Machine>,
}

impl Latch {
    /// The local server, which needs no resolving.
    pub fn local(sink: Sink, pane: String) -> Self {
        Self {
            sink,
            pane,
            occupant: Occupant::Other,
            machine: None,
        }
    }
}

impl Sink {
    pub fn deliver(&self, pane: &str, text: &str, submit: bool) -> Result<Delivery> {
        match self {
            Self::Local(client) => {
                client.send_text(pane, text)?;
                if submit {
                    client.send_keys(pane, &["enter"])?;
                    return Ok(Delivery::Submitted);
                }
                Ok(Delivery::Typed)
            }
            Self::Remote(target) => deliver_remote(target, pane, text, submit),
        }
    }

    pub fn set_label(&self, pane: &str, label: &str, ttl: Duration) -> Result<()> {
        match self {
            Self::Local(client) => client.set_pane_label(pane, label, ttl),
            Self::Remote(target) => {
                let script = remote::label_script(target, pane, Some((label, ttl)));
                remote::run(&target.ssh, &script).map(|_| ())
            }
        }
    }

    pub fn clear_label(&self, pane: &str) -> Result<()> {
        match self {
            Self::Local(client) => client.clear_pane_label(pane),
            Self::Remote(target) => {
                let script = remote::label_script(target, pane, None);
                remote::run(&target.ssh, &script).map(|_| ())
            }
        }
    }

    pub fn ttl(&self) -> Duration {
        match self {
            Self::Local(_) => LOCAL_TTL,
            Self::Remote(_) => REMOTE_TTL,
        }
    }

    /// A remote refresh costs a round trip, so it runs rarely enough that a
    /// long dictation does not become a stream of ssh calls.
    pub fn refresh(&self) -> Duration {
        match self {
            Self::Local(_) => LOCAL_REFRESH,
            Self::Remote(_) => REMOTE_REFRESH,
        }
    }

    pub fn describe(&self) -> &str {
        match self {
            Self::Local(_) => "local",
            Self::Remote(target) => &target.label,
        }
    }
}

fn deliver_remote(target: &Target, pane: &str, text: &str, submit: bool) -> Result<Delivery> {
    if text.contains('\0') {
        return Err(Error::Remote {
            label: target.label.clone(),
            message: "transcript contains a NUL".into(),
        });
    }
    let script = remote::deliver_script(target, pane, text, submit);

    let mut output = remote::run(&target.ssh, &script)?;
    if output.is_transport_failure() {
        // A shared connection can go stale between dictations; ssh reconnects
        // on its own, so one retry is the whole recovery.
        std::thread::sleep(Duration::from_millis(200));
        output = remote::run(&target.ssh, &script)?;
    }

    match output.code {
        0 if submit => Ok(Delivery::Submitted),
        0 => Ok(Delivery::Typed),
        remote::AGENT_GONE => Ok(Delivery::AgentGone),
        code => Err(Error::Remote {
            label: target.label.clone(),
            message: format!("delivery exited {code}: {}", output.stderr.trim()),
        }),
    }
}

/// The machine the sidebar points at, resolved far enough to deliver to, or
/// `None` when the dictation belongs to the local server.
///
/// Runs before recording starts, so an unreachable machine costs an error
/// rather than a lost transcript.
pub fn selected(config: &settings::Remote) -> Result<Option<Latch>> {
    if !config.enabled {
        return Ok(None);
    }
    let herdr = machine::herdr_bin(&config.herdr);
    let Some(machine) = machine::selected(&herdr)? else {
        return Ok(None);
    };
    if !machine.enabled {
        return Err(Error::Remote {
            label: machine.label,
            message: "the selected machine is disabled".into(),
        });
    }

    let ssh = remote::Ssh {
        program: config.ssh.clone(),
        destination: machine.target.clone(),
        control: remote::control_path(&machine.target),
        connect_timeout: config.connect_timeout(),
        control_persist: Duration::from_secs(config.control_persist_secs),
        timeout: config.timeout(),
    };

    let nonce = remote::nonce();
    let configured = config.machine(&machine.id, &machine.label).herdr;
    let script = remote::resolve_script(&machine.session, &configured, &nonce);
    let output = remote::run(&ssh, &script)?;
    if output.code != 0 && output.stdout.trim().is_empty() {
        return Err(Error::Remote {
            label: machine.label,
            message: format!("could not be reached: {}", output.stderr.trim()),
        });
    }
    let probe = remote::parse_probe(&output.stdout, &nonce)?;

    Ok(Some(Latch {
        pane: probe.focused_pane.clone(),
        occupant: probe.occupant(),
        sink: Sink::Remote(Box::new(Target {
            ssh,
            herdr: probe.herdr,
            session: machine.session.clone(),
            label: machine.label.clone(),
        })),
        machine: Some(machine),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            ssh: remote::Ssh {
                program: "ssh".into(),
                destination: "host".into(),
                control: None,
                connect_timeout: Duration::from_secs(10),
                control_persist: Duration::from_secs(300),
                timeout: Duration::from_secs(20),
            },
            herdr: "/home/u/.local/bin/herdr".into(),
            session: "default".into(),
            label: "desk".into(),
        }
    }

    #[test]
    fn submitting_prefers_the_agent_surface() {
        let script = remote::deliver_script(&target(), "w1:p3", "hello", true);
        assert!(script.contains("agent prompt"));
        assert!(script.contains("agent list"));
        assert!(script.contains("exit 3"));
    }

    #[test]
    fn not_submitting_never_reaches_the_agent_surface() {
        // agent prompt submits; asking for text without Enter must not.
        let script = remote::deliver_script(&target(), "w1:p3", "hello", false);
        assert!(!script.contains("agent prompt"));
        assert!(script.contains("pane send-text"));
    }

    #[test]
    fn a_transcript_cannot_escape_its_quotes() {
        let script = remote::deliver_script(&target(), "w1:p3", "it's $(id); rm -rf /", true);
        assert!(script.contains(r#"T='it'\''s $(id); rm -rf /'"#));
        assert!(!script.contains("\nrm -rf"));
    }

    #[test]
    fn a_transcript_with_a_nul_is_refused() {
        let err = deliver_remote(&target(), "w1:p3", "a\0b", true).unwrap_err();
        assert!(err.to_string().contains("NUL"));
    }

    #[test]
    fn a_remote_indicator_refreshes_rarely_enough_to_stay_cheap() {
        let remote = Sink::Remote(Box::new(target()));
        assert!(remote.refresh() * 2 < remote.ttl());
        assert_eq!(remote.describe(), "desk");
    }

    #[test]
    fn a_disabled_machine_is_refused_rather_than_delivered_locally() {
        // The whole point: never silently fall back to the local server.
        let config = settings::Remote {
            enabled: false,
            ..settings::Remote::default()
        };
        assert!(selected(&config).unwrap().is_none());
    }
}
