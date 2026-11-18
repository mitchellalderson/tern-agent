import 'models.dart';

/// A document an agent wrote (docs/documents-design.md): a real file in a
/// workspace plus a log of revisions.
class DocumentMeta {
  const DocumentMeta({
    required this.id,
    required this.title,
    required this.kind,
    required this.path,
    required this.rev,
    required this.updatedAt,
    this.bytes = 0,
    this.tainted = false,
    this.agentId,
    this.sessionId,
    this.workspaceId = '',
  });

  factory DocumentMeta.fromJson(Json j) => DocumentMeta(
    id: j['id'] as String,
    title: j['title'] as String,
    kind: j['kind'] as String? ?? 'markdown',
    path: j['path'] as String? ?? '',
    rev: j['rev'] as int? ?? 1,
    bytes: j['bytes'] as int? ?? 0,
    tainted: j['tainted'] as bool? ?? false,
    agentId: j['agent_id'] as String?,
    sessionId: j['session_id'] as String?,
    workspaceId: j['workspace_id'] as String? ?? '',
    updatedAt: DateTime.fromMillisecondsSinceEpoch(
      j['updated_at'] as int? ?? 0,
    ),
  );

  final String id;
  final String title;

  /// `markdown`, `text`, `code` or `csv`.
  final String kind;

  /// Relative to the workspace.
  final String path;
  final int rev;
  final int bytes;

  /// From web or other untrusted content, not yet reviewed.
  final bool tainted;
  final String? agentId;
  final String? sessionId;
  final String workspaceId;
  final DateTime updatedAt;
}

/// A document with its latest text.
class DocumentContent {
  const DocumentContent({required this.meta, required this.content});

  factory DocumentContent.fromJson(Json j) => DocumentContent(
    meta: DocumentMeta.fromJson(j),
    content: j['content'] as String? ?? '',
  );

  final DocumentMeta meta;
  final String content;
}

/// One saved state of a document.
class DocumentRevision {
  const DocumentRevision({
    required this.rev,
    required this.authorKind,
    required this.authorId,
    required this.createdAt,
    this.summary,
    this.added = 0,
    this.removed = 0,
    this.bytes = 0,
    this.tainted = false,
    this.rebased = false,
    this.runId,
    this.content,
  });

  factory DocumentRevision.fromJson(Json j) => DocumentRevision(
    rev: j['rev'] as int,
    authorKind: j['author_kind'] as String? ?? 'agent',
    authorId: j['author_id'] as String? ?? '',
    summary: j['summary'] as String?,
    added: j['added'] as int? ?? 0,
    removed: j['removed'] as int? ?? 0,
    bytes: j['bytes'] as int? ?? 0,
    tainted: j['tainted'] as bool? ?? false,
    rebased: j['rebased'] as bool? ?? false,
    runId: j['run_id'] as String?,
    content: j['content'] as String?,
    createdAt: DateTime.fromMillisecondsSinceEpoch(
      j['created_at'] as int? ?? 0,
    ),
  );

  final int rev;

  /// `agent`, `user`, `external` or `system`.
  final String authorKind;
  final String authorId;
  final String? summary;
  final int added;
  final int removed;
  final int bytes;
  final bool tainted;
  final bool rebased;
  final String? runId;
  final DateTime createdAt;

  /// Set when fetched singly.
  final String? content;
}
