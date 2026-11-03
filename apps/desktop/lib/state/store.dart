import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:tern_client/tern_client.dart';

import 'notifier.dart';
import 'transcript.dart';

enum Link { connecting, online, offline }

/// Everything the app knows, fed by the daemon. Widgets listen to it.
///
/// The daemon owns all state: this is a cache rebuilt on every (re)connect,
/// since the event stream has no replay.
class TernStore extends ChangeNotifier {
  TernStore(this.client, {Notifier? notifier})
    : notifier = notifier ?? const SilentNotifier() {
    this.notifier.onAction = _onNotification;
  }

  final TernClient client;
  final Notifier notifier;

  /// Whether the window has focus; notifications only go out when it
  /// doesn't (or when it's showing another conversation).
  bool appActive = true;

  Link link = Link.connecting;
  String? linkError;
  Health? health;
  Info? info;
  List<SessionItem> sessions = [];

  /// The open conversation. Null with [draft] true means a new one that
  /// hasn't been sent yet.
  String? selected;
  bool draft = true;
  final transcripts = <String, Transcript>{};
  final _runSession = <String, String>{};

  /// Sub-agent run → the run that started it.
  final _parentOf = <String, String>{};

  /// Run → the conversation its approvals show in (a sub-agent's go to
  /// the one the user started).
  final _approvalSession = <String, String>{};
  final _secretSession = <String, String>{};
  final usage = <String, Usage>{};
  List<Schedule> schedules = [];

  /// Model lists per provider, loaded on demand by the pickers.
  List<ProviderModels>? modelLists;
  bool loadingModels = false;

  /// The model a new conversation will use, when chosen before sending.
  String? draftModel;
  String? error;
  var _badge = 0;

  StreamSubscription<TernEvent>? _events;
  Timer? _retry;
  var _backoff = const Duration(milliseconds: 500);
  var _disposed = false;

  Transcript? get current => selected == null ? null : transcripts[selected];

  bool get running => current?.activeRun != null;

  /// Approvals waiting anywhere, for badges.
  int get pendingApprovals => transcripts.values
      .expand((t) => t.items)
      .whereType<ApprovalItem>()
      .where((a) => a.outcome == null)
      .length;

  /// Secret requests waiting anywhere.
  int get pendingSecrets => transcripts.values
      .expand((t) => t.items)
      .whereType<SecretRequestItem>()
      .where((r) => r.open)
      .length;

  /// Whether the core can ask for secrets (older ones can't).
  bool get hasSecretRequests =>
      health?.features.contains('secret_requests') ?? false;

  String? modelFor(String? session) {
    if (session == null) return draftModel ?? info?.defaultModel;
    final s = sessions.where((x) => x.id == session).firstOrNull;
    return s?.model ?? info?.defaultModel;
  }

  /// No usable model yet: show provider setup instead of the composer.
  bool get setupNeeded => info?.setupNeeded ?? false;

  Future<void> connect() async {
    _retry?.cancel();
    await _events?.cancel();
    _events = null;
    if (link != Link.online) {
      link = Link.connecting;
      notifyListeners();
    }
    try {
      health = await client.health();
      info = await client.info();
      // Subscribe before resyncing, so nothing falls between the two.
      _events = client.events().listen(
        _onEvent,
        onError: (Object e) => _lost(e),
        onDone: () => _lost(null),
      );
      await _resync();
      link = Link.online;
      linkError = null;
      _backoff = const Duration(milliseconds: 500);
    } on TernException catch (e) {
      _lost(e);
      return;
    }
    notifyListeners();
  }

  void _lost(Object? e) {
    if (_disposed) return;
    _events?.cancel();
    _events = null;
    link = Link.offline;
    linkError = e?.toString() ?? 'connection closed';
    notifyListeners();
    _retry?.cancel();
    _retry = Timer(_backoff, connect);
    final next = _backoff * 2;
    _backoff = next > const Duration(seconds: 10)
        ? const Duration(seconds: 10)
        : next;
  }

  Future<void> _resync() async {
    sessions = await client.sessions();
    schedules = await client.schedules();
    final active = await client.activeRuns();
    for (final r in active) {
      _runSession[r.runId] = r.sessionId;
      if (r.parentRunId case final p?) _parentOf[r.runId] = p;
    }
    if (selected case final id?) {
      await _load(id);
    }
    for (final r in active) {
      transcripts[r.sessionId]?.activeRun = r.runId;
    }
    for (final a in await client.pendingApprovals()) {
      _transcriptFor(a.sessionId)?.apply(ApprovalRequested(a));
    }
    if (hasSecretRequests) {
      try {
        for (final r in await client.secretRequests()) {
          _routeSecret(r);
          _transcriptFor(r.rootSessionId ?? r.sessionId)
              ?.apply(SecretRequested(r));
        }
      } on TernException {
        // Not available to this device: the event stream still delivers.
      }
    }
  }

  Future<void> _load(String id) async {
    final detail = await client.session(id);
    final previous = transcripts[id];
    final t = Transcript.fromSession(detail)
      ..activeRun = previous?.activeRun
      ..runSince = previous?.runSince;
    // Keep approval cards: the stored history doesn't contain them.
    for (final a
        in previous?.items.whereType<ApprovalItem>() ?? <ApprovalItem>[]) {
      if (a.outcome == null) t.items.add(a);
    }
    for (final r
        in previous?.items.whereType<SecretRequestItem>() ??
            <SecretRequestItem>[]) {
      if (r.open) t.items.add(r);
    }
    transcripts[id] = t;
    usage[id] = await client.usage(id);
    if (t.children.isNotEmpty) {
      try {
        final runs = await client.subagents(id);
        for (final r in runs) {
          _runSession[r.runId] = r.sessionId;
          _parentOf[r.runId] = r.parentRunId;
        }
        t.mergeChildren(runs);
      } on TernException {
        // A daemon without sub-agents: the calls alone will do.
      }
    }
  }

  /// Load a sub-agent's conversation, to show inside its parent's.
  Future<void> loadChild(String session) async {
    try {
      await _load(session);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// Stop one sub-agent (and any of its own).
  Future<void> cancelRun(String run) async {
    try {
      await client.cancel(run);
    } on TernException catch (e) {
      error = e.message;
      notifyListeners();
    }
  }

  /// Say something to a running sub-agent. Returns the daemon's reason on
  /// failure.
  Future<String?> messageRun(String run, String text) async {
    try {
      await client.messageRun(run, text);
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// The transcript holding [run]'s agents card, if [run] is a sub-agent.
  Transcript? _parentTranscript(String run) => switch (_parentOf[run]) {
    final p? => switch (_runSession[p]) {
      final s? => transcripts[s],
      null => null,
    },
    null => null,
  };

  Transcript? _transcriptFor(String session) =>
      transcripts.putIfAbsent(session, Transcript.new);

  @visibleForTesting
  void onEventForTest(TernEvent e) => _onEvent(e);

  void _onEvent(TernEvent e) {
    String? session;
    switch (e) {
      case ApprovalRequested(:final approval):
        session = approval.rootSessionId ?? approval.sessionId;
        _runSession[approval.runId] ??= approval.sessionId;
        _approvalSession[approval.runId] = session;
      case SecretRequested(:final request):
        session = _routeSecret(request);
      case RunStarted(:final runId, :final sessionId):
        session = sessionId;
        _runSession[runId] = sessionId;
        if (e.parentRunId case final p?) _parentOf[runId] = p;
      case SubagentSpawned(
        :final runId,
        :final childRunId,
        :final childSessionId,
      ):
        session = e.sessionId ?? _runSession[runId];
        _runSession[childRunId] = childSessionId;
        _parentOf[childRunId] = runId;
      case ApprovalAnswered(:final runId) when _approvalSession[runId] != null:
        session = _approvalSession[runId];
      case RunEvent(:final runId, :final sessionId):
        session = sessionId ?? _runSession[runId];
        if (sessionId != null) _runSession[runId] = sessionId;
        if (e.parentRunId case final p?) _parentOf[runId] ??= p;
      case ProvidersChanged():
        unawaited(refreshInfo());
        return;
      case ModelsChanged():
        // Only re-fetch lists someone is looking at.
        if (modelLists != null) unawaited(loadModels());
        return;
      case UnknownEvent():
        return;
    }
    if (session == null) {
      // A run we haven't seen start; learn where it belongs.
      unawaited(_learnRuns());
      return;
    }
    var changed = _transcriptFor(session)!.apply(e);
    if (e is ToolFinished) changed = _closeSecrets(e) || changed;
    if (e is RunEvent && _parentOf.containsKey(e.runId)) {
      changed = _onChildEvent(e, session) || changed;
    } else if (e is RunFinished) {
      unawaited(_afterRun(session, e));
    }
    if (e is ApprovalRequested) _notifyApproval(e.approval);
    if (e is SecretRequested) _notifySecret(e.request, session);
    if (changed) notifyListeners();
  }

  /// Reflect a sub-agent's progress on its parent's agents card.
  bool _onChildEvent(RunEvent e, String session) {
    final parent = _parentTranscript(e.runId);
    switch (e) {
      case RunStarted(:final runId):
        return parent?.updateChild(runId, status: 'running') ?? false;
      case RunFinished(:final runId, :final status, :final error):
        // Its approvals live in the conversation the user started.
        if (_approvalSession[runId] case final root?) {
          transcripts[root]?.expireApprovals(runId);
        }
        if (_secretSession[runId] case final root?) {
          transcripts[root]?.expireSecretRequests(runId);
        }
        final reply = transcripts[session]?.items
            .whereType<AssistantItem>()
            .lastOrNull
            ?.text;
        return parent?.updateChild(
              runId,
              status: switch (status) {
                'interrupted' when error != null => 'cancelled',
                _ => status,
              },
              result: reply,
              error: error,
              finished: DateTime.now(),
            ) ??
            false;
      default:
        // Its own transcript changed; the card shows that too.
        return parent != null;
    }
  }

  /// Notify only when the user can't already see it.
  bool _unseen(String session) => !appActive || selected != session;

  void _notifyApproval(Approval a) {
    if (!_unseen(a.sessionId)) return;
    unawaited(
      notifier.show(
        id: a.id,
        title: '${a.agentLabel ?? 'tern'} wants to run ${a.tool}',
        body: a.summary,
        approval: true,
      ),
    );
  }

  /// Where a request's card belongs: the conversation the user started.
  String _routeSecret(SecretRequest r) {
    final session = r.rootSessionId ?? r.sessionId;
    _runSession[r.runId] ??= r.sessionId;
    _secretSession[r.runId] = session;
    return session;
  }

  /// A `secret_request` call ended: its card closes, whoever answered.
  /// (A sub-agent's event lands in its own transcript; the card is in the
  /// root's.)
  bool _closeSecrets(ToolFinished e) {
    var changed = false;
    for (final t in transcripts.values) {
      changed = t.closeSecretRequests(e.runId, e.id, e.output) || changed;
    }
    return changed;
  }

  // The notification names the request, never a value field.
  void _notifySecret(SecretRequest r, String session) {
    if (!_unseen(session)) return;
    final names = r.secrets.map((s) => s.name).join(', ');
    unawaited(
      notifier.show(
        id: r.id,
        title: '${r.agentLabel ?? 'The agent'} is asking for a secret',
        body: names,
      ),
    );
  }

  void _notifyFinished(String session, RunFinished e) {
    if (!_unseen(session) || e.status == 'interrupted') return;
    final title = sessions.where((s) => s.id == session).firstOrNull?.title;
    final reply = transcripts[session]?.items
        .whereType<AssistantItem>()
        .lastOrNull
        ?.text
        .trim()
        .split('\n')
        .first;
    unawaited(
      notifier.show(
        id: session,
        title: title ?? 'tern',
        body: e.status == 'failed'
            ? 'Failed: ${e.error ?? 'unknown error'}'
            : reply ?? 'Done',
      ),
    );
  }

  void _onNotification(String id, NotificationAction action) {
    if (action == NotificationAction.open) {
      // An approval id opens the conversation it belongs to.
      final session =
          transcripts.entries
              .where(
                (t) => t.value.items.any(
                  (i) =>
                      (i is ApprovalItem && i.approval.id == id) ||
                      (i is SecretRequestItem && i.request.id == id),
                ),
              )
              .map((t) => t.key)
              .firstOrNull ??
          id;
      unawaited(open(session));
      return;
    }
    final item = transcripts.values
        .expand((t) => t.items)
        .whereType<ApprovalItem>()
        .where((a) => a.approval.id == id && a.outcome == null)
        .firstOrNull;
    if (item != null) {
      unawaited(
        answer(
          item,
          action == NotificationAction.allow ? Decision.once : Decision.deny,
        ),
      );
    }
  }

  @override
  void notifyListeners() {
    final pending = pendingApprovals + pendingSecrets;
    if (pending != _badge) {
      _badge = pending;
      unawaited(notifier.badge(pending));
    }
    super.notifyListeners();
  }

  Future<void> _learnRuns() async {
    try {
      for (final r in await client.activeRuns()) {
        _runSession[r.runId] = r.sessionId;
        _transcriptFor(r.sessionId)!.activeRun = r.runId;
      }
      notifyListeners();
    } on TernException {
      // The event stream will report the connection problem.
    }
  }

  Future<void> _afterRun(String session, RunFinished e) async {
    try {
      sessions = await client.sessions();
      usage[session] = await client.usage(session);
      if (schedules.any((s) => s.sessionId == session)) {
        schedules = await client.schedules();
      }
      _notifyFinished(session, e);
      notifyListeners();
    } on TernException {
      // Refreshed again on reconnect.
    }
  }

  Future<void> open(String id) async {
    selected = id;
    draft = false;
    error = null;
    notifyListeners();
    try {
      await _load(id);
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  void newConversation() {
    selected = null;
    draft = true;
    draftModel = null;
    error = null;
    notifyListeners();
  }

  Future<void> send(String text) async {
    error = null;
    final session = selected;
    if (session != null) {
      _transcriptFor(session)!.items.add(UserItem(text));
      notifyListeners();
    }
    try {
      final started = await client.send(
        text,
        session: session,
        model: session == null ? draftModel : null,
      );
      _runSession[started.runId] = started.sessionId;
      if (session == null) {
        // Events may have arrived for the new session already.
        final t = _transcriptFor(started.sessionId)!;
        t.items.insert(0, UserItem(text));
        selected = started.sessionId;
        draft = false;
        draftModel = null;
        sessions = await client.sessions();
      }
      _transcriptFor(started.sessionId)!.activeRun ??= started.runId;
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  Future<void> stop() async {
    final run = current?.activeRun;
    if (run == null) return;
    try {
      await client.cancel(run);
    } on TernException catch (e) {
      error = e.message;
      notifyListeners();
    }
  }

  Future<void> resume(String run) async {
    try {
      await client.resume(run);
      current?.items.removeWhere((i) => i is RunEndItem && i.runId == run);
      current?.activeRun = run;
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  Future<void> answer(ApprovalItem item, Decision d) async {
    try {
      await client.answer(item.approval.id, d);
      item.outcome = switch (d) {
        Decision.once => 'approved',
        Decision.always => 'always allowed',
        Decision.deny => 'denied',
      };
    } on TernException catch (e) {
      // Usually: the run already moved on and the approval expired.
      item.outcome = 'expired';
      error = e.message;
    }
    notifyListeners();
  }

  /// Send what the user entered (only non-empty values; an empty map
  /// skips the request). The values live in the card's fields and the
  /// request body, nowhere else.
  Future<void> answerSecret(
    SecretRequestItem item,
    Map<String, String> values,
  ) async {
    try {
      await client.answerSecretRequest(item.request.id, values);
      item.saved = values.keys.toList();
      item.outcome = values.isEmpty ? 'skipped' : 'saved';
    } on TernException catch (e) {
      switch (e.status) {
        case 403:
          item.readOnly = true;
        case 404:
          // Answered elsewhere, or the run moved on.
          item.outcome = 'expired';
        default:
          error = e.message;
      }
    }
    notifyListeners();
  }

  Future<void> refreshSchedules() async {
    try {
      schedules = await client.schedules();
    } on TernException catch (e) {
      error = e.message;
    }
    notifyListeners();
  }

  /// Create or edit a routine. Returns the daemon's reason on failure.
  Future<String?> saveSchedule({
    String? id,
    required String cron,
    required String prompt,
    String? deliverTo,
  }) async {
    try {
      if (id == null) {
        await client.createSchedule(
          cron: cron,
          prompt: prompt,
          deliverTo: deliverTo,
        );
      } else {
        await client.updateSchedule(
          id,
          cron: cron,
          prompt: prompt,
          deliverTo: deliverTo ?? 'none',
        );
      }
      await refreshSchedules();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  Future<void> setScheduleEnabled(Schedule s, bool enabled) async {
    try {
      await client.updateSchedule(s.id, enabled: enabled);
    } on TernException catch (e) {
      error = e.message;
    }
    await refreshSchedules();
  }

  Future<void> deleteSchedule(Schedule s) async {
    try {
      await client.deleteSchedule(s.id);
    } on TernException catch (e) {
      error = e.message;
    }
    await refreshSchedules();
  }

  /// Run a routine now and open its conversation.
  Future<void> runSchedule(Schedule s) async {
    try {
      final started = await client.runSchedule(s.id);
      _runSession[started.runId] = started.sessionId;
      _transcriptFor(started.sessionId)!.activeRun = started.runId;
      sessions = await client.sessions();
      await open(started.sessionId);
    } on TernException catch (e) {
      error = e.message;
      notifyListeners();
    }
  }

  /// Validate and set the model for the open conversation.
  /// Use `model` for this conversation (or the next new one), and with
  /// `makeDefault` for new conversations too. Returns the daemon's reason
  /// on failure; a warning (unlisted model) lands in [modelWarning].
  Future<String?> setModel(String model, {bool makeDefault = false}) async {
    try {
      final r = await client.checkModel(model);
      modelWarning = r.warning;
      if (selected case final id?) {
        await client.setModel(id, r.model);
        sessions = await client.sessions();
      } else {
        draftModel = r.model;
      }
      if (makeDefault) {
        await client.setDefaultModel(r.model);
        await refreshInfo(notify: false);
      }
      notifyListeners();
      return null;
    } on TernException catch (e) {
      return e.message;
    }
  }

  /// Set when the last model chosen isn't in its provider's list.
  String? modelWarning;

  /// Re-read the default model and whether setup is needed.
  Future<void> refreshInfo({bool notify = true}) async {
    try {
      info = await client.info();
    } on TernException catch (e) {
      error = e.message;
    }
    if (notify) notifyListeners();
  }

  /// Fetch every provider's models (cached by the daemon unless `refresh`).
  Future<void> loadModels({bool refresh = false}) async {
    loadingModels = true;
    notifyListeners();
    try {
      modelLists = await client.models(refresh: refresh);
    } on TernException catch (e) {
      error = e.message;
      modelLists ??= [];
    } finally {
      loadingModels = false;
      if (!_disposed) notifyListeners();
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _retry?.cancel();
    _events?.cancel();
    client.close();
    super.dispose();
  }
}
