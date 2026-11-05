import 'models.dart';

/// What the OS does for shell commands in a folder (the header chip).
enum ShellState {
  /// A verified OS sandbox: commands change files only in the folder.
  sandboxed,

  /// The folder can't be written, so there is nothing for commands to change.
  readOnly,

  /// Docker, or the sandbox isn't confirmed yet.
  unverified,

  /// No OS sandbox: a command can read or change anything you can. Tern asks
  /// before every one and never runs them unattended.
  unbounded;

  static ShellState parse(String? s) => switch (s) {
    'sandboxed' => sandboxed,
    'read_only' => readOnly,
    'unverified' => unverified,
    _ => unbounded,
  };
}

/// A read-only extra folder (or single file) of a workspace.
class WorkspaceRoot {
  const WorkspaceRoot({
    required this.path,
    this.kind = 'user',
    this.isFile = false,
  });

  factory WorkspaceRoot.fromJson(Json j) => WorkspaceRoot(
    path: j['path'] as String,
    kind: j['kind'] as String? ?? 'user',
    isFile: j['is_file'] as bool? ?? false,
  );

  final String path;

  /// `user` or `git_dir`.
  final String kind;
  final bool isFile;
}

/// What uses a workspace.
class WorkspaceUsedBy {
  const WorkspaceUsedBy({
    this.channels = const [],
    this.sessions = 0,
    this.agents = const [],
    this.routines = const [],
  });

  factory WorkspaceUsedBy.fromJson(Json j) => WorkspaceUsedBy(
    channels: [for (final c in j['channels'] as List? ?? const []) c as String],
    sessions: j['sessions'] as int? ?? 0,
    agents: [for (final a in j['agents'] as List? ?? const []) a as String],
    routines: [for (final r in j['routines'] as List? ?? const []) r as String],
  );

  final List<String> channels;
  final int sessions;
  final List<String> agents;
  final List<String> routines;

  bool get isEmpty =>
      channels.isEmpty && sessions == 0 && agents.isEmpty && routines.isEmpty;
}

/// A folder an agent can reach, with its status, read-only extras and what
/// uses it.
class Workspace {
  const Workspace({
    required this.id,
    required this.name,
    required this.path,
    this.isDefault = false,
    this.status = 'ok',
    this.statusDetail,
    this.network,
    this.writable = true,
    this.shellState = ShellState.unbounded,
    this.roots = const [],
    this.usedBy = const WorkspaceUsedBy(),
  });

  factory Workspace.fromJson(Json j) => Workspace(
    id: j['id'] as String,
    name: j['name'] as String,
    path: j['path'] as String,
    isDefault: j['is_default'] as bool? ?? false,
    status: j['status'] as String? ?? 'ok',
    statusDetail: j['status_detail'] as String?,
    network: j['network'] as String?,
    writable: j['writable'] as bool? ?? true,
    shellState: ShellState.parse(j['shell_state'] as String?),
    roots: [
      for (final r in j['roots'] as List? ?? const [])
        WorkspaceRoot.fromJson(r as Json),
    ],
    usedBy: WorkspaceUsedBy.fromJson(j['used_by'] as Json? ?? const {}),
  );

  final String id;
  final String name;
  final String path;
  final bool isDefault;

  /// `ok`, `missing`, `replaced`, `denied` or `invalid`.
  final String status;
  final String? statusDetail;

  /// `off` when this folder is stricter than the global network setting.
  final String? network;
  final bool writable;
  final ShellState shellState;
  final List<WorkspaceRoot> roots;
  final WorkspaceUsedBy usedBy;

  bool get isOk => status == 'ok';
}

/// The verdict on a folder: the words to show, nothing saved.
class WorkspaceVerdict {
  const WorkspaceVerdict({
    this.canonical,
    this.shortcutOf,
    this.kind = 'dir',
    this.errors = const [],
    this.warnings = const [],
    this.suggestedRoot,
    this.denied = false,
  });

  factory WorkspaceVerdict.fromJson(Json j) => WorkspaceVerdict(
    canonical: j['canonical'] as String?,
    shortcutOf: j['shortcut_of'] as String?,
    kind: j['kind'] as String? ?? 'dir',
    errors: [for (final e in j['errors'] as List? ?? const []) e as String],
    warnings: [for (final w in j['warnings'] as List? ?? const []) w as String],
    suggestedRoot: j['suggested_root'] as String?,
    denied: j['denied'] as bool? ?? false,
  );

  /// The path it resolves to, which is what is stored.
  final String? canonical;

  /// The path as chosen, when it is a shortcut to [canonical].
  final String? shortcutOf;

  /// `dir` or `file`.
  final String kind;

  /// Why it can't be used. Empty means it can.
  final List<String> errors;

  /// Things to confirm first.
  final List<String> warnings;

  /// For a folder inside a repository: the repository folder to use instead.
  final String? suggestedRoot;

  /// macOS didn't let the daemon open it (Files and Folders permission).
  final bool denied;

  bool get isOk => errors.isEmpty && canonical != null;
}

/// A folder Tern refused (HTTP 422): [verdict] says why.
class WorkspaceRefused implements Exception {
  const WorkspaceRefused(this.verdict, this.message);
  final WorkspaceVerdict verdict;
  final String message;

  @override
  String toString() => message;
}

/// What a session, channel or agent is bound to.
class WorkspaceBinding {
  const WorkspaceBinding({
    this.workspace,
    required this.effective,
    required this.source,
  });

  factory WorkspaceBinding.fromJson(Json j) => WorkspaceBinding(
    workspace: j['workspace'] == null
        ? null
        : Workspace.fromJson(j['workspace'] as Json),
    effective: Workspace.fromJson(j['effective'] as Json),
    source: j['source'] as String? ?? 'default',
  );

  /// Its own binding; null when it inherits.
  final Workspace? workspace;

  /// The folder a run would get now.
  final Workspace effective;

  /// `session`, `channel`, `agent` or `default`.
  final String source;
}
