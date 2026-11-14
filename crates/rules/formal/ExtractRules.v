From Rocq2Rust Require Import Loader.
Require Import InputRules Schedule ObservationRules AccessibilityRules LockedUseRequirement.

Rocq2Rust Export Library "rules.rs"
  InputRules.validate_plan InputRules.authorize InputRules.bind_destination InputRules.apply_transition InputRules.finish_releases InputRules.new_pointer_presses
  LockedUseRequirement.confirmation_matches
  Schedule.sample_times
  ObservationRules.group_frames ObservationRules.local_change ObservationRules.same_scroll
  ObservationRules.pack_images
  AccessibilityRules.retained_read AccessibilityRules.reference_live AccessibilityRules.authorize_insertion.
