From Stdlib Require Import Bool List NArith.
Import ListNotations.
Open Scope N_scope.

(* A desktop script can prepare to listen before triggering playback, read
   finalized speech incrementally, and stop while retaining the last phrase.
   The macOS adapter supplies PCM from the selected process. The decoder
   supplies the segments and their timing. These native claims are checked at
   the MCP boundary with a real playback fixture. *)

(* One run can expose one observation of a selected audio process. Locked Use
   authorizes window-scoped effects, so it cannot admit a process audio tap. *)
Definition audio_available (locked_use_enabled : bool) : bool :=
  negb locked_use_enabled.

Inductive start_decision := Start | AlreadyStarted | StartCancelled.

Definition decide_start (occupied cancelled : bool) : start_decision :=
  if occupied then AlreadyStarted else if cancelled then StartCancelled else Start.

Theorem managed_run_cannot_observe_audio :
  audio_available true = false.
Proof. reflexivity. Qed.

Theorem audio_start_contract occupied cancelled :
  decide_start occupied cancelled = Start <->
  occupied = false /\ cancelled = false.
Proof. destruct occupied, cancelled; simpl; split; intros; intuition discriminate. Qed.

(* Bounds are in milliseconds and apply at the script API before native work. *)
Definition listen_duration_allowed (ms : N) : bool :=
  (1 <=? ms) && (ms <=? 30000).

Definition transcript_wait_allowed (ms : N) : bool :=
  ms <=? 5000.

Theorem listen_duration_contract ms :
  listen_duration_allowed ms = true <-> 1 <= ms <= 30000.
Proof. unfold listen_duration_allowed. now rewrite andb_true_iff, !N.leb_le. Qed.

Theorem transcript_wait_contract ms :
  transcript_wait_allowed ms = true <-> ms <= 5000.
Proof. apply N.leb_le. Qed.

(* On a graceful stop, the tap is stopped once and captured PCM is drained.
   Cancellation or decoder failure instead aborts the observation. Rust
   supplies deadlines and native acknowledgements and owns the tap. *)
Inductive capture_phase := Capturing | Draining.
Inductive capture_action := AbortCapture | StopAndDrain | ReadCapturedPcm.

Definition capture_action_for phase (abort stop_or_deadline : bool) :=
  if abort then AbortCapture
  else match phase with
       | Capturing => if stop_or_deadline then StopAndDrain else ReadCapturedPcm
       | Draining => ReadCapturedPcm
       end.

Theorem capture_cancel_aborts phase stop_or_deadline :
  capture_action_for phase true stop_or_deadline = AbortCapture.
Proof. reflexivity. Qed.

Theorem capture_stop_drains :
  capture_action_for Capturing false true = StopAndDrain /\
  forall stop_or_deadline,
    capture_action_for Draining false stop_or_deadline = ReadCapturedPcm.
Proof. split; reflexivity. Qed.

(* A worker sends ordered final segments, then one finalization event after
   capture and the decoder have settled. The abstract segment may contain
   any text and timestamps supplied by the native decoder. *)
Inductive transcript_status (Error : Type) :=
  | Receiving | Finalized | TranscriptFailed (why : Error).
Arguments Receiving {Error}.
Arguments Finalized {Error}.
Arguments TranscriptFailed {Error} _.

Inductive transcript_event (Segment Error : Type) :=
  | SegmentReady (segment : Segment)
  | WorkerFinished (result : option Error).
Arguments SegmentReady {Segment Error} _.
Arguments WorkerFinished {Segment Error} _.

(* The input batch is consumed by one read. A terminal event is sticky
   across later reads and cannot expose events after the end of the stream. *)
Fixpoint drain_transcript {Segment Error}
    (status : transcript_status Error) (events : list (transcript_event Segment Error))
    : transcript_status Error * list Segment :=
  match status, events with
  | Receiving, SegmentReady segment :: tail =>
      let '(next, segments) := drain_transcript Receiving tail in
      (next, segment :: segments)
  | Receiving, WorkerFinished None :: _ => (Finalized, [])
  | Receiving, WorkerFinished (Some why) :: _ => (TranscriptFailed why, [])
  | _, _ => (status, [])
  end.

(* Splitting events into any number of reads preserves the final status and
   the drained segment order when each batch is supplied once. The script
   boundary propagates a worker failure as a rejected read. *)
Theorem transcript_drain_respects_batches {Segment Error} (status : transcript_status Error)
    (first later : list (transcript_event Segment Error)) :
  drain_transcript status (first ++ later) =
  let '(middle, earlier) := drain_transcript status first in
  let '(final, following) := drain_transcript middle later in
  (final, earlier ++ following).
Proof.
  destruct status.
  - induction first as [| event first IH].
    + simpl. now destruct (drain_transcript Receiving later).
    + destruct event as [segment | [failure |]].
      * simpl. rewrite IH.
        destruct (drain_transcript Receiving first) as [middle earlier].
        now destruct (drain_transcript middle later).
      * destruct later; reflexivity.
      * destruct later; reflexivity.
  - destruct first, later; reflexivity.
  - destruct first, later; reflexivity.
Qed.

Theorem transcript_finalization_returns_prefix {Segment Error} (segments : list Segment)
    (later : list (transcript_event Segment Error)) :
  drain_transcript Receiving (map SegmentReady segments ++ WorkerFinished None :: later) =
  (Finalized, segments).
Proof.
  induction segments as [| segment segments IH].
  - reflexivity.
  - simpl. now rewrite IH.
Qed.

Inductive read_intent := Poll | WaitForFirst | StopAndFinalize.
Inductive read_action := Reply | WaitForSegment | WaitForFinalization.

Definition read_intent_for (stop can_wait : bool) :=
  if stop then StopAndFinalize else if can_wait then WaitForFirst else Poll.

Definition read_action_for intent (receiving has_segments : bool) :=
  if receiving
  then match intent with
       | Poll => Reply
       | WaitForFirst => if has_segments then Reply else WaitForSegment
       | StopAndFinalize => WaitForFinalization
       end
  else Reply.

Theorem stopping_waits_for_finalization has_segments :
  read_action_for StopAndFinalize true has_segments = WaitForFinalization /\
  read_action_for StopAndFinalize false has_segments = Reply.
Proof. split; reflexivity. Qed.
