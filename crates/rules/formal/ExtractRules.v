From Rocq2Rust Require Import Loader.
Require Import InputRules Schedule ObservationRules AccessibilityRules LockedUseRequirement AudioRules.

Rocq2Rust Export Library "rules.rs"
  InputRules.validate_plan InputRules.authorize InputRules.bind_destination InputRules.apply_transition InputRules.finish_releases InputRules.new_pointer_presses
  LockedUseRequirement.confirmation_matches
  Schedule.sample_times
  ObservationRules.group_frames ObservationRules.local_change ObservationRules.same_scroll
  ObservationRules.pack_images
  AccessibilityRules.retained_read AccessibilityRules.reference_live AccessibilityRules.authorize_insertion
  AccessibilityRules.choose_window
  AudioRules.audio_available AudioRules.decide_start AudioRules.listen_duration_allowed
  AudioRules.transcript_wait_allowed AudioRules.capture_action_for
  AudioRules.drain_transcript AudioRules.read_intent_for AudioRules.read_action_for.
