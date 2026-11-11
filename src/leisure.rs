use crate::model::MowerStatus;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LeisureConfig {
    pub enabled: bool,
    pub min_idle_minutes: u32,
    pub warning_minutes: u32,
}

impl Default for LeisureConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_idle_minutes: 30,
            warning_minutes: 5,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Alert {
    Available,
    ReturnSoon,
    TakingOver,
    Due,
    Uncertain,
}

#[derive(Default)]
pub struct LeisureState {
    pub active: bool,
    previous: Option<MowerStatus>,
    eligible: bool,
    warned: bool,
    online: bool,
}

impl LeisureState {
    pub fn update(
        &mut self,
        settings: &LeisureConfig,
        status: MowerStatus,
        online: bool,
        remaining: Option<i64>,
    ) -> Option<Alert> {
        if !settings.enabled {
            *self = Self::default();
            return None;
        }
        if !online {
            let alert = (self.active && self.online).then_some(Alert::Uncertain);
            self.online = false;
            if !self.active {
                self.previous = None;
                self.eligible = false;
            }
            return alert;
        }
        self.online = true;
        if status != MowerStatus::Sleeping {
            let alert = if self.active {
                if status == MowerStatus::Working && !self.warned {
                    Some(Alert::TakingOver)
                } else if status == MowerStatus::Working {
                    None
                } else {
                    Some(Alert::Uncertain)
                }
            } else {
                None
            };
            self.active = false;
            self.eligible = false;
            self.warned = false;
            self.previous = Some(status);
            return alert;
        }
        if self.previous != Some(MowerStatus::Sleeping) {
            self.eligible = self.previous == Some(MowerStatus::Working);
            self.active = false;
            self.warned = false;
        }
        self.previous = Some(status);
        let Some(seconds) = remaining.filter(|seconds| *seconds > 0) else {
            let alert = self.active.then_some(if remaining.is_some() {
                Alert::Due
            } else {
                Alert::Uncertain
            });
            self.active = false;
            self.eligible = false;
            return alert;
        };
        let minimum = settings.min_idle_minutes.clamp(2, 1440);
        let warning = settings.warning_minutes.clamp(1, minimum - 1) as i64 * 60;
        if self.eligible && !self.active && seconds >= minimum as i64 * 60 {
            self.active = true;
            return Some(Alert::Available);
        }
        if self.active && !self.warned && seconds <= warning {
            self.warned = true;
            return Some(Alert::ReturnSoon);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MowerStatus::*;

    fn settings() -> LeisureConfig {
        LeisureConfig {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn qualifies_only_after_work_and_warns_once() {
        let mut state = LeisureState::default();
        let cfg = settings();
        assert_eq!(state.update(&cfg, Sleeping, true, Some(3600)), None);
        state.update(&cfg, Working, true, Some(0));
        assert_eq!(
            state.update(&cfg, Sleeping, true, Some(1800)),
            Some(Alert::Available)
        );
        assert!(state.active);
        assert_eq!(state.update(&cfg, Sleeping, true, Some(1700)), None);
        assert_eq!(
            state.update(&cfg, Sleeping, true, Some(300)),
            Some(Alert::ReturnSoon)
        );
        assert_eq!(state.update(&cfg, Sleeping, true, Some(100)), None);
    }

    #[test]
    fn short_gap_and_stopped_are_not_play_windows() {
        let mut state = LeisureState::default();
        let cfg = settings();
        state.update(&cfg, Working, true, None);
        assert_eq!(state.update(&cfg, Sleeping, true, Some(600)), None);
        assert!(!state.active);
        state.update(&cfg, Stopped, true, None);
        assert_eq!(state.update(&cfg, Sleeping, true, Some(3600)), None);
    }

    #[test]
    fn earlier_schedule_warns_immediately_and_early_work_cancels_window() {
        let mut state = LeisureState::default();
        let cfg = settings();
        state.update(&cfg, Working, true, None);
        state.update(&cfg, Sleeping, true, Some(3600));
        assert_eq!(
            state.update(&cfg, Sleeping, true, Some(60)),
            Some(Alert::ReturnSoon)
        );
        state.update(&cfg, Working, true, None);
        state.update(&cfg, Sleeping, true, Some(3600));
        assert_eq!(
            state.update(&cfg, Working, true, None),
            Some(Alert::TakingOver)
        );
        assert!(!state.active);
    }

    #[test]
    fn disconnect_reports_uncertainty_without_repeating_available_on_reconnect() {
        let mut state = LeisureState::default();
        let cfg = settings();
        state.update(&cfg, Working, true, None);
        state.update(&cfg, Sleeping, true, Some(3600));
        assert_eq!(
            state.update(&cfg, Sleeping, false, Some(3500)),
            Some(Alert::Uncertain)
        );
        assert_eq!(state.update(&cfg, Sleeping, false, Some(3400)), None);
        assert_eq!(state.update(&cfg, Sleeping, true, Some(3300)), None);
        assert_eq!(
            state.update(&cfg, Sleeping, true, None),
            Some(Alert::Uncertain)
        );
        assert!(!state.active);
    }

    #[test]
    fn disabled_mode_resets_state() {
        let mut state = LeisureState::default();
        let cfg = settings();
        state.update(&cfg, Working, true, None);
        state.update(&cfg, Sleeping, true, Some(3600));
        state.update(&LeisureConfig::default(), Sleeping, true, Some(3500));
        assert!(!state.active);
    }

    #[test]
    fn missing_observations_do_not_fabricate_a_completed_round() {
        let mut state = LeisureState::default();
        let cfg = settings();
        state.update(&cfg, Working, true, None);
        state.update(&cfg, Working, false, None);
        assert_eq!(state.update(&cfg, Sleeping, true, Some(3600)), None);
        assert!(!state.active);
    }

    #[test]
    fn postponed_task_does_not_repeat_the_warning() {
        let mut state = LeisureState::default();
        let cfg = settings();
        state.update(&cfg, Working, true, None);
        state.update(&cfg, Sleeping, true, Some(3600));
        assert_eq!(
            state.update(&cfg, Sleeping, true, Some(300)),
            Some(Alert::ReturnSoon)
        );
        assert_eq!(state.update(&cfg, Sleeping, true, Some(3600)), None);
        assert_eq!(state.update(&cfg, Sleeping, true, Some(300)), None);
    }

    #[test]
    fn reaching_deadline_does_not_claim_task_execution() {
        let mut state = LeisureState::default();
        let cfg = settings();
        state.update(&cfg, Working, true, None);
        state.update(&cfg, Sleeping, true, Some(3600));
        assert_eq!(
            state.update(&cfg, Sleeping, true, Some(0)),
            Some(Alert::Due)
        );
        assert!(!state.active);
    }
}
