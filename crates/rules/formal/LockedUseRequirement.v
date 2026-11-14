From Stdlib Require Import Arith.

(** A broker reply for an ordinary run carries no native-lock claim.
    A protected run may return success only with a relock confirmation.
    Unrecognized replies never enter this decision. *)
Inductive run_mode := Ordinary | Protected.
Inductive finish_reply := Ok | Locked.

Definition confirmation_matches mode reply : bool :=
  match mode, reply with
  | Ordinary, Ok | Protected, Locked => true
  | Ordinary, Locked | Protected, Ok => false
  end.

Theorem confirmation_matches_contract mode reply :
  confirmation_matches mode reply = true <->
  (mode = Ordinary /\ reply = Ok) \/ (mode = Protected /\ reply = Locked).
Proof.
  destruct mode, reply; simpl; intuition discriminate.
Qed.

(** A protected MCP script continues in the user's original desktop session
    after the OS unlocks it. Session identity includes the login instance.
    A fresh login under the same account can have different windows and state.

    A signed broker authorized during setup owns OS authentication, display
    and physical-input shielding, and watchdog recovery. Selvedge consumes
    a lease scoped to one script and receives only authorization and status
    evidence. Neither credentials nor per-run prompts belong to that protocol.

    Positions order observable transitions. In particular, protection must
    hold at the effect itself. The broker and platform must keep permission,
    dispatch and revocation consistent across the check-to-effect interval.
    Polling just before dispatch establishes an observation at that check. *)
Section ProtectedRun.
  Context {Session Lease : Type}.

  Inductive EffectPurpose := ScriptEffect | ReleaseHeldInput.

  Inductive StopReason :=
  | ScriptSucceeded
  | ScriptFailed
  | UserTakeover
  | AuthorizationLost.

  Record Run := {
    original_session : Session;
    script_lease : Lease;
    requested_at : nat;
    admitted_at : option nat;
    authorization_expires_at : nat;
    stopped_at : nat;
    stop_reason : StopReason
  }.

  Record PlatformEvidence := {
    installed_authority : Session -> Prop;
    active_session : nat -> Session;
    natively_locked : Session -> nat -> Prop;
    display_shielded : Session -> nat -> Prop;
    physical_input_shielded : Session -> nat -> Prop;
    needs_local_participation : Lease -> nat -> Prop;
    authorized_lease : Session -> nat -> option Lease;
    permitted_effect : EffectPurpose -> nat -> Prop;
    local_takeover : Session -> nat -> Prop;
    native_unlock_by_user : Session -> nat -> Prop;
    relock_requested : Session -> Lease -> nat -> Prop;
    relock_acknowledged : Session -> Lease -> nat -> Prop
  }.

  Record RuntimeEvidence := {
    committed_effect : Lease -> EffectPurpose -> nat -> Prop;
    operations_drained : Lease -> nat -> Prop;
    held_controls_released : Lease -> nat -> Prop;
    reported_protected_success : Lease -> nat -> Prop
  }.

  Context (platform : PlatformEvidence) (runtime : RuntimeEvidence).

  Definition shielded session position : Prop :=
    display_shielded platform session position /\
    physical_input_shielded platform session position.

  Definition original_desktop_ready run position : Prop :=
    active_session platform position = original_session run /\
    ~ natively_locked platform (original_session run) position /\
    shielded (original_session run) position.

  Definition admitted run position : Prop :=
    admitted_at run = Some position /\
    requested_at run < position /\ position < stopped_at run /\
    original_desktop_ready run position /\
    authorized_lease platform (original_session run) position = Some (script_lease run).

  (** The same guard applies to input, accessibility and screen capture.
      Script effects stop at cancellation or revocation. Releasing a held
      control can require another native effect, with its own live permission. *)
  Definition effect_authorized run purpose position : Prop :=
    (exists start, admitted run start /\ start <= position) /\
    (purpose = ScriptEffect ->
      position < stopped_at run /\ position < authorization_expires_at run) /\
    original_desktop_ready run position /\
    authorized_lease platform (original_session run) position = Some (script_lease run) /\
    permitted_effect platform purpose position.

  Definition relocked run position : Prop :=
    natively_locked platform (original_session run) position /\
    relock_acknowledged platform (original_session run) (script_lease run) position.

  (** The broker maintains protection across the entire episode, including
      unlock and shutdown. A takeover signal can reach the broker while
      shielding still prevents physical input from reaching the desktop. *)
  Definition broker_protects_until_relocked run restored : Prop :=
    installed_authority platform (original_session run) /\
    natively_locked platform (original_session run) (requested_at run) /\
    requested_at run < restored /\ stopped_at run <= restored /\
    relocked run restored /\
    forall position, requested_at run <= position < restored ->
      (natively_locked platform (original_session run) position \/
       shielded (original_session run) position) /\
      ~ needs_local_participation platform (script_lease run) position.

  (** The broker observes either the local takeover event or the
      deadline independently of the process executing the script. It
      requests relock on stop; confirmation can require OS recovery.
      A native manual unlock after recovery starts ordinary use. *)
  Definition stop_revokes_automation run : Prop :=
    stopped_at run <= authorization_expires_at run /\
    (stop_reason run = UserTakeover ->
      local_takeover platform (original_session run) (stopped_at run)) /\
    (forall position,
      requested_at run <= position <= stopped_at run ->
      local_takeover platform (original_session run) position ->
      position = stopped_at run /\ stop_reason run = UserTakeover) /\
    relock_requested platform (original_session run) (script_lease run) (stopped_at run) /\
    forall position, stopped_at run <= position ->
      ~ permitted_effect platform ScriptEffect position.

  Definition restored_or_manually_unlocked run restored reply : Prop :=
    forall position, restored <= position <= reply ->
      natively_locked platform (original_session run) position \/
      exists local, restored < local <= position /\
        native_unlock_by_user platform (original_session run) local.

  Definition drained run position : Prop :=
    operations_drained runtime (script_lease run) position /\
    forall purpose effect,
      committed_effect runtime (script_lease run) purpose effect -> effect < position.

  (** Cooperative success closes the lease after all work and held input have
      drained. On failure or takeover, the watchdog may relock first. Any
      unacknowledged input release or relock prevents a protected success. *)
  Definition completed run restored reply : Prop :=
    stop_reason run = ScriptSucceeded /\
    exists start finish,
      admitted run start /\
      stopped_at run <= finish /\ finish < restored /\ restored < reply /\
      drained run finish /\ held_controls_released runtime (script_lease run) finish /\
      restored_or_manually_unlocked run restored reply.

  (** For the Rust runtime, LockedUse::acquire establishes admission and
      DesktopPermission::check guards native effects. LockedUse::finish
      belongs after the work and input drain and before the successful MCP
      reply. The broker supplies the physical guarantee even if the Rust
      process disappears. This contract states obligations on actual traces.
      Rocq checks their implication below, and a broker/OS acceptance run is
      needed to check that the platform establishes the premises. *)
  Definition protected_run_contract run : Prop :=
    exists restored,
      broker_protects_until_relocked run restored /\ stop_revokes_automation run /\
      (forall purpose position,
        committed_effect runtime (script_lease run) purpose position ->
        position < restored /\ effect_authorized run purpose position) /\
      (forall reply, reported_protected_success runtime (script_lease run) reply ->
        completed run restored reply).

  Theorem successful_reply_follows_relock_and_all_effects run reply :
    protected_run_contract run ->
    reported_protected_success runtime (script_lease run) reply ->
    exists restored,
      restored < reply /\ natively_locked platform (original_session run) restored /\
      forall purpose effect,
        committed_effect runtime (script_lease run) purpose effect -> effect < restored.
  Proof.
    intros [restored [protection [_ [effects reports]]]] success.
    destruct protection as [_ [_ [_ [_ [[locked _] _]]]]].
    destruct (reports reply success) as [_ [start [finish completed_run]]].
    destruct completed_run as [_ [_ [_ [lock_before_reply _]]]].
    exists restored.
    split; [exact lock_before_reply |].
    split; [exact locked |].
    intros purpose effect committed. exact (proj1 (effects purpose effect committed)).
  Qed.
End ProtectedRun.
