//! What state a plugin is in, worked out from its folder and nothing else.
//!
//! The folder is the truth: `<store>/<id>/`, and the id is the folder's name. Nothing is recorded anywhere else that
//! could disagree with the disk, so a plugin can never be installed and invisible at once. Every folder has a state, and
//! every state has a way out (see `docs/plugin-installation-design.txt`).
//!
//! The rule is split in two so that it can be tried without a disk. [`classify`] is pure: it is given what was found and
//! says the state, in a fixed order. [`Found`] is what is found, and gathering it (reading the folder) is a separate step
//! that decides nothing.
//!
//! THE ORDER IS THE POINT. The first that applies wins, and the order is the order of how much it matters to the person:
//! a plugin that cannot run at all is broken whatever else is true; one that was never approved is not run, however fine
//! its files are; one whose program is not the one that was approved is not run either; and only then does it matter that
//! it was switched off.

/// What was found in a plugin's folder. Each field is a fact about the disk, not a judgement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The folder itself, and the store it sits in, can be changed only by this user and are this user's.
    pub folder: Folder,
    /// The folder holds a manifest the application can read and accepts.
    pub manifest: Manifest,
    /// The program the manifest names is there and can be run (a plain file, runnable, owned by this user, not
    /// writable by others).
    pub program: Program,
    /// What the person approved, if anything usable was found of it.
    pub approval: Approval,
    /// The marker that says the person switched it off is present.
    pub disabled: bool,
}

/// What was found of the folder itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Folder {
    /// Only this user can change it, and it is this user's.
    Safe,
    /// Group or others can change it, or it is someone else's, or it cannot be read.
    NotSafe,
}

/// What was found of the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manifest {
    /// Readable and accepted.
    Good,
    /// Absent, unreadable, not valid, or one the application refuses.
    Unusable,
    /// The manifest is valid, but explicitly excludes this operating system.
    UnsupportedPlatform,
}

/// What was found of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// There, and fit to run.
    Good,
    /// Absent, not a plain file, not runnable, or one that others could change or that is not this user's.
    Unusable,
}

/// What was found of the person's approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approval {
    /// There is no approval, or it could not be read or is not valid: it is as if the person had not approved.
    None,
    /// An approval, as it was written.
    Given(Given),
}

/// What an approval says, compared with what is on disk NOW.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Given {
    /// The program that is there now is the one that was approved (the digest of its bytes and of whether it can be run
    /// is the one recorded).
    pub program_unchanged: bool,
    /// Every capability the manifest asks for is among those that were approved.
    pub covers_what_is_asked: bool,
}

/// The state of a plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// It cannot run, whatever the person decides: no usable manifest, or no program fit to run. The only way out is to
    /// remove it.
    Broken(Why),
    /// It was never approved, or its approval was lost or damaged, or the manifest now asks for more than was approved.
    /// It is not run until the person looks at what it asks for and approves.
    NeedsApproval,
    /// The program is not the one that was approved. It is not run.
    Changed,
    /// The person switched it off. It stays, keeps what it was granted, and is not run.
    Disabled,
    /// Everything agrees: it may be run.
    Ready,
}

/// Why a plugin is broken, in words this application chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The folder, or the store it is in, can be changed by someone other than this user, or is not this user's.
    NotSafe,
    NoManifest,
    NoProgram,
    UnsupportedPlatform,
}

impl Why {
    /// A fixed sentence. Nothing read from the folder is ever repeated, because what a file says is the plugin's own text.
    pub fn words(self) -> &'static str {
        match self {
            Why::NotSafe => "Its folder can be changed by others, so it is not run",
            Why::NoManifest => "It has no manifest the application can use",
            Why::NoProgram => "It has no program the application can run",
            Why::UnsupportedPlatform => "It does not support this operating system",
        }
    }
}

impl State {
    /// Whether the application may start it.
    pub fn may_run(self) -> bool {
        self == State::Ready
    }
}

/// The state of a plugin that was found as `found`, by the first of these that applies:
/// broken, needs approval, changed, disabled, ready.
pub fn classify(found: &Found) -> State {
    // First of all, and whatever else is true: what is run is whatever is at the program's path when it is started, and
    // anyone who can change the folder can put something else there after everything below was looked at. Nothing else in
    // the folder, the manifest included, can be trusted either.
    if found.folder == Folder::NotSafe {
        return State::Broken(Why::NotSafe);
    }
    if found.manifest == Manifest::Unusable {
        return State::Broken(Why::NoManifest);
    }
    if found.manifest == Manifest::UnsupportedPlatform {
        return State::Broken(Why::UnsupportedPlatform);
    }
    if found.program == Program::Unusable {
        return State::Broken(Why::NoProgram);
    }
    let Approval::Given(given) = &found.approval else {
        return State::NeedsApproval;
    };
    // Asking for more than was approved is a new question, whatever else is true; it is checked before the program, since a
    // program that changed AND a manifest that grew are both reasons to ask, and asking is the one that can be answered.
    if !given.covers_what_is_asked {
        return State::NeedsApproval;
    }
    if !given.program_unchanged {
        return State::Changed;
    }
    if found.disabled {
        return State::Disabled;
    }
    State::Ready
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Found {
        Found {
            folder: Folder::Safe,
            manifest: Manifest::Good,
            program: Program::Good,
            approval: Approval::Given(Given {
                program_unchanged: true,
                covers_what_is_asked: true,
            }),
            disabled: false,
        }
    }

    #[test]
    fn a_folder_where_everything_agrees_is_ready_and_may_run() {
        assert_eq!(classify(&good()), State::Ready);
        assert!(classify(&good()).may_run());
    }

    #[test]
    fn only_a_ready_plugin_may_run() {
        for state in [
            State::Broken(Why::NotSafe),
            State::Broken(Why::NoManifest),
            State::Broken(Why::NoProgram),
            State::NeedsApproval,
            State::Changed,
            State::Disabled,
        ] {
            assert!(!state.may_run(), "{state:?} was allowed to run");
        }
        assert!(State::Ready.may_run());
    }

    #[test]
    fn a_missing_manifest_or_program_is_broken_whatever_else_is_true() {
        // Even with an approval that would otherwise be fine, and even switched off: a plugin that cannot run has no use for
        // the rest of its state, and the person's only way out of it is to remove it.
        for disabled in [false, true] {
            let mut found = good();
            found.disabled = disabled;
            found.manifest = Manifest::Unusable;
            assert_eq!(classify(&found), State::Broken(Why::NoManifest));
            let mut found = good();
            found.disabled = disabled;
            found.program = Program::Unusable;
            assert_eq!(classify(&found), State::Broken(Why::NoProgram));
        }
        // With both gone, the manifest is named: it is the first thing that was looked for.
        let mut found = good();
        found.manifest = Manifest::Unusable;
        found.program = Program::Unusable;
        assert_eq!(classify(&found), State::Broken(Why::NoManifest));
        // And with no approval either, it is still broken and not "needs approval": approving a plugin that cannot run
        // would be a question with no answer.
        found.approval = Approval::None;
        assert_eq!(classify(&found), State::Broken(Why::NoManifest));
    }

    #[test]
    fn no_approval_means_needs_approval_not_blocked() {
        // A lost, damaged or never-given approval costs a question, never a block.
        let mut found = good();
        found.approval = Approval::None;
        assert_eq!(classify(&found), State::NeedsApproval);
        // Also when it was switched off: it is first of all not approved.
        found.disabled = true;
        assert_eq!(classify(&found), State::NeedsApproval);
    }

    #[test]
    fn a_manifest_that_asks_for_more_than_was_approved_needs_approval_again() {
        let mut found = good();
        found.approval = Approval::Given(Given {
            program_unchanged: true,
            covers_what_is_asked: false,
        });
        assert_eq!(classify(&found), State::NeedsApproval);
        // Even if the program also changed: the question that can be answered is asked first.
        found.approval = Approval::Given(Given {
            program_unchanged: false,
            covers_what_is_asked: false,
        });
        assert_eq!(classify(&found), State::NeedsApproval);
    }

    #[test]
    fn a_program_that_is_not_the_approved_one_is_changed_and_not_run() {
        let mut found = good();
        found.approval = Approval::Given(Given {
            program_unchanged: false,
            covers_what_is_asked: true,
        });
        assert_eq!(classify(&found), State::Changed);
        assert!(!classify(&found).may_run());
        // Switched off does not hide it: a changed program is what the person must hear about.
        found.disabled = true;
        assert_eq!(classify(&found), State::Changed);
    }

    #[test]
    fn a_plugin_that_was_switched_off_is_disabled_only_when_everything_else_agrees() {
        let mut found = good();
        found.disabled = true;
        assert_eq!(classify(&found), State::Disabled);
        assert!(!classify(&found).may_run());
    }

    #[test]
    fn the_sentences_for_a_broken_plugin_are_fixed_and_all_differ() {
        let all = [
            Why::NotSafe,
            Why::NoManifest,
            Why::NoProgram,
            Why::UnsupportedPlatform,
        ];
        for (index, one) in all.iter().enumerate() {
            assert!(!one.words().is_empty());
            for other in &all[index + 1..] {
                assert_ne!(
                    one.words(),
                    other.words(),
                    "{one:?} and {other:?} say the same thing"
                );
            }
        }
    }

    #[test]
    fn a_folder_others_can_change_is_broken_before_anything_else_is_looked_at() {
        // Whatever else is true, even a perfect and approved plugin: what runs is whatever is at the program's path when it
        // is started, and anyone who can change the folder can put another program there afterwards.
        let mut found = good();
        found.folder = Folder::NotSafe;
        assert_eq!(classify(&found), State::Broken(Why::NotSafe));
        // And it is named first even when the manifest and program are also unusable, and with no approval, or switched off:
        // the folder cannot be trusted to say anything at all.
        found.manifest = Manifest::Unusable;
        found.program = Program::Unusable;
        found.approval = Approval::None;
        found.disabled = true;
        assert_eq!(classify(&found), State::Broken(Why::NotSafe));
    }

    #[test]
    fn every_combination_of_what_can_be_found_has_exactly_one_state_and_only_agreement_runs() {
        // 2 folders x 3 manifest states x 2 programs x 5 approvals x 2 disabled:
        // every one is classified, and a plugin runs only where the folder is safe, the manifest and program are good, the
        // approval covers everything asked, the program is the approved one and it is not switched off.
        let approvals = [
            Approval::None,
            Approval::Given(Given {
                program_unchanged: false,
                covers_what_is_asked: false,
            }),
            Approval::Given(Given {
                program_unchanged: false,
                covers_what_is_asked: true,
            }),
            Approval::Given(Given {
                program_unchanged: true,
                covers_what_is_asked: false,
            }),
            Approval::Given(Given {
                program_unchanged: true,
                covers_what_is_asked: true,
            }),
        ];
        let mut ready = 0;
        let mut total = 0;
        for folder in [Folder::Safe, Folder::NotSafe] {
            for manifest in [
                Manifest::Good,
                Manifest::Unusable,
                Manifest::UnsupportedPlatform,
            ] {
                for program in [Program::Good, Program::Unusable] {
                    for approval in &approvals {
                        for disabled in [false, true] {
                            let found = Found {
                                folder,
                                manifest,
                                program,
                                approval: approval.clone(),
                                disabled,
                            };
                            let state = classify(&found);
                            let should_run = folder == Folder::Safe
                                && manifest == Manifest::Good
                                && program == Program::Good
                                && *approval
                                    == Approval::Given(Given {
                                        program_unchanged: true,
                                        covers_what_is_asked: true,
                                    })
                                && !disabled;
                            assert_eq!(state.may_run(), should_run, "{found:?} was {state:?}");
                            ready += usize::from(state == State::Ready);
                            total += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(total, 120);
        assert_eq!(ready, 1, "exactly one combination may run");
    }
}
