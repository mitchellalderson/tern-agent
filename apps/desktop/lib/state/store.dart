import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:tern_client/tern_client.dart';

import 'transcript.dart';

enum Link { connecting, online, offline }

/// Everything the app knows, fed by the daemon. Widgets listen to it.
///
/// The daemon owns all state: this is a cache rebuilt on every (re)connect,
/// since the event stream has no replay.
class TernStore extends ChangeNotifier {
  TernStore(this.client);

  final TernClient client;

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
  final usage = <String, Usage>{};
  String? error;

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

  String? modelFor(String? session) {
    final s = sessions.where((x) => x.id == session).firstOrNull;
    return s?.model ?? info?.defaultModel;
  }

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
    final active = await client.activeRuns();
    for (final r in active) {
      _runSession[r.runId] = r.sessionId;
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
  }

  Future<void> _load(String id) async {
    final detail = await client.session(id);
    final previous = transcripts[id];
    final t = Transcript.fromSession(detail)..activeRun = previous?.activeRun;
    // Keep approval cards: the stored history doesn't contain them.
    for (final a
        in previous?.items.whereType<ApprovalItem>() ?? <ApprovalItem>[]) {
      if (a.outcome == null) t.items.add(a);
    }
    transcripts[id] = t;
    usage[id] = await client.usage(id);
  }

  Transcript? _transcriptFor(String session) =>
      transcripts.putIfAbsent(session, Transcript.new);

  void _onEvent(TernEvent e) {
    String? session;
    switch (e) {
      case ApprovalRequested(:final approval):
        session = approval.sessionId;
        _runSession[approval.runId] = session;
      case RunStarted(:final runId, :final sessionId):
        session = sessionId;
        _runSession[runId] = sessionId;
      case RunEvent(:final runId):
        session = _runSession[runId];
      case UnknownEvent():
        return;
    }
    if (session == null) {
      // A run we haven't seen start; learn where it belongs.
      unawaited(_learnRuns());
      return;
    }
    final changed = _transcriptFor(session)!.apply(e);
    if (e is RunFinished) unawaited(_afterRun(session));
    if (changed) notifyListeners();
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

  Future<void> _afterRun(String session) async {
    try {
      sessions = await client.sessions();
      usage[session] = await client.usage(session);
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
      final started = await client.send(text, session: session);
      _runSession[started.runId] = started.sessionId;
      if (session == null) {
        // Events may have arrived for the new session already.
        final t = _transcriptFor(started.sessionId)!;
        t.items.insert(0, UserItem(text));
        selected = started.sessionId;
        draft = false;
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

  /// Validate and set the model for the open conversation.
  Future<String?> setModel(String model) async {
    try {
      await client.checkModel(model);
      if (selected case final id?) {
        await client.setModel(id, model);
        sessions = await client.sessions();
        notifyListeners();
      }
      return null;
    } on TernException catch (e) {
      return e.message;
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
