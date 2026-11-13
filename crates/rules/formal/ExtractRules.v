From Rocq2Rust Require Import Loader.
Require Import InputRules Schedule ObservationRules AccessibilityRules.

Rocq2Rust Export Library "rules.rs"
  InputRules.validate_plan InputRules.authorize InputRules.bind_destination InputRules.apply_transition InputRules.finish_releases InputRules.new_pointer_presses
  Schedule.sample_times
  ObservationRules.group_frames ObservationRules.local_change ObservationRules.same_scroll
  ObservationRules.delivery_plan
  AccessibilityRules.retained_read AccessibilityRules.reference_live AccessibilityRules.authorize_insertion.
