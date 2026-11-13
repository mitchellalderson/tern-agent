import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:tern_client/tern_client.dart';

/// A `create_document` or `edit_document` call that finished: which document
/// and where. Parsed from the tool's JSON output.
class DocumentTouch {
  const DocumentTouch({
    required this.docId,
    required this.rev,
    required this.created,
    required this.runId,
    this.channelId,
  });

  static DocumentTouch? fromToolOutput(
    String output, {
    required bool created,
    required String runId,
    String? channelId,
  }) {
    try {
      final j = jsonDecode(output);
      if (j is! Map || j['doc_id'] is! String) return null;
      return DocumentTouch(
        docId: j['doc_id'] as String,
        rev: (j['rev'] as num?)?.toInt() ?? 1,
        created: created,
        runId: runId,
        channelId: channelId,
      );
    } on FormatException {
      return null;
    }
  }

  final String docId;
  final int rev;
  final bool created;
  final String runId;
  final String? channelId;
}

/// What the Documents panel needs from the core; [DocumentsState] only sees
/// this, so tests can stand in for the daemon.
abstract class DocumentsApi {
  Future<List<DocumentMeta>> list();
  Future<DocumentContent> get(String id);
  Future<List<DocumentRevision>> revisions(String id);
  Future<DocumentRevision> revision(String id, int rev);
  Future<void> markReviewed(String id);
}

class ClientDocumentsApi implements DocumentsApi {
  ClientDocumentsApi(this._client);
  final TernClient _client;

  @override
  Future<List<DocumentMeta>> list() => _client.documents();
  @override
  Future<DocumentContent> get(String id) => _client.document(id);
  @override
  Future<List<DocumentRevision>> revisions(String id) =>
      _client.documentRevisions(id);
  @override
  Future<DocumentRevision> revision(String id, int rev) =>
      _client.documentRevision(id, rev);
  @override
  Future<void> markReviewed(String id) => _client.markDocumentReviewed(id);
}

/// The panel's tabs and what they show. Read-only: documents are written by
/// agents; the panel loads them, follows their changes and lists revisions.
class DocumentsState extends ChangeNotifier {
  DocumentsState(this._api);

  final DocumentsApi _api;

  /// Every document the core knows, newest first.
  List<DocumentMeta> all = const [];

  /// Open tabs, in order.
  final List<String> open = [];
  String? selected;

  final Map<String, DocumentContent> _content = {};
  final Map<String, List<DocumentRevision>> _revisions = {};

  /// Tabs the agent changed while another one was in front.
  final Set<String> unseen = {};

  /// An older revision being looked at (read-only), per document.
  final Map<String, DocumentRevision> _viewing = {};

  /// Who changed a document last and when it was noticed, for the status
  /// line ("Changed by research, just now").
  final Map<String, DateTime> changedAt = {};

  String? error;
  bool loading = false;
  bool _disposed = false;

  DocumentContent? contentOf(String id) => _content[id];
  List<DocumentRevision>? revisionsOf(String id) => _revisions[id];

  /// The older revision on show for [id], or null for the latest.
  DocumentRevision? viewing(String id) => _viewing[id];

  DocumentMeta? meta(String id) =>
      _content[id]?.meta ?? all.where((d) => d.id == id).firstOrNull;

  Future<void> refresh() async {
    loading = true;
    _notify();
    try {
      all = await _api.list();
      error = null;
    } on Object catch (e) {
      error = '$e';
    }
    loading = false;
    _notify();
  }

  /// Open [id] in a tab and bring it to the front.
  Future<void> openDoc(String id) async {
    if (!open.contains(id)) open.add(id);
    selected = id;
    unseen.remove(id);
    _notify();
    await _load(id);
  }

  void select(String id) {
    if (!open.contains(id)) return;
    selected = id;
    unseen.remove(id);
    _notify();
  }

  void close(String id) {
    final at = open.indexOf(id);
    if (at < 0) return;
    open.removeAt(at);
    _viewing.remove(id);
    unseen.remove(id);
    if (selected == id) {
      selected = open.isEmpty ? null : open[at.clamp(0, open.length - 1)];
    }
    _notify();
  }

  /// The agent wrote [touch]: refresh the list and the open tab, and open a
  /// new document when [openNew].
  Future<void> touched(DocumentTouch touch, {required bool openNew}) async {
    changedAt[touch.docId] = DateTime.now();
    if (touch.created && openNew) {
      await openDoc(touch.docId);
    } else if (open.contains(touch.docId)) {
      if (selected != touch.docId) unseen.add(touch.docId);
      // Looking at an older revision stays put; the latest is reloaded.
      await _load(touch.docId);
    }
    await refresh();
  }

  Future<void> _load(String id) async {
    try {
      final c = await _api.get(id);
      _content[id] = c;
      _revisions[id] = await _api.revisions(id);
      error = null;
    } on Object catch (e) {
      error = '$e';
    }
    _notify();
  }

  /// Show an older revision read-only; null goes back to the latest.
  Future<void> viewRevision(String id, int? rev) async {
    if (rev == null || rev == _content[id]?.meta.rev) {
      _viewing.remove(id);
      _notify();
      return;
    }
    try {
      _viewing[id] = await _api.revision(id, rev);
      error = null;
    } on Object catch (e) {
      error = '$e';
    }
    _notify();
  }

  Future<void> markReviewed(String id) async {
    try {
      await _api.markReviewed(id);
      await _load(id);
      await refresh();
    } on Object catch (e) {
      error = '$e';
      _notify();
    }
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
