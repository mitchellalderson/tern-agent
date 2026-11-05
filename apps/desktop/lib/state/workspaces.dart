// The workspace (folder) interface the desktop UI is built on. It mirrors the
// daemon API of docs/alpha-workspaces.md (section 4, API; section 5,
// validation). The UI only ever sees [WorkspaceApi]; the one file that talks
// to the core is workspaces_adapter.dart.

import 'package:tern_client/tern_client.dart'
    show WorkspaceVerdict, WorkspaceRefused;

export 'package:tern_client/tern_client.dart' show WorkspaceRefused;

/// A refusal carrying just the core's wording.
WorkspaceRefused refusal(String message) =>
    WorkspaceRefused(WorkspaceVerdict(errors: [message]), message);

/// What a folder binding is attached to.
enum WorkspaceKind { session, channel, agent }

class WorkspaceTarget {
  const WorkspaceTarget(this.kind, this.id);
  const WorkspaceTarget.session(this.id) : kind = WorkspaceKind.session;
  const WorkspaceTarget.channel(this.id) : kind = WorkspaceKind.channel;
  const WorkspaceTarget.agent(this.id) : kind = WorkspaceKind.agent;

  final WorkspaceKind kind;
  final String id;

  @override
  bool operator ==(Object other) =>
      other is WorkspaceTarget && other.kind == kind && other.id == id;

  @override
  int get hashCode => Object.hash(kind, id);
}

/// Where a run's folder comes from, nearest first.
enum WorkspaceOrigin { session, channel, agent, global }

/// Can the folder be used right now? `denied` is a macOS privacy refusal
/// (TCC) seen by the daemon; `replaced` is a different directory at the same
/// path.
enum WorkspaceStatus { ok, missing, replaced, denied }

/// What the core can promise about shell commands here (section 8).
enum ShellBound { bounded, unverified, unbounded }

/// The UI's view of tern_client's `Workspace` (see workspaces_adapter.dart).
class WorkspaceView {
  const WorkspaceView({
    required this.id,
    required this.path,
    this.name,
    this.status = WorkspaceStatus.ok,
    this.shellBound = ShellBound.unbounded,
    this.extraRoots = const [],
    this.usedBy = const [],
  });

  final String id;
  final String path;
  final String? name;
  final WorkspaceStatus status;
  final ShellBound shellBound;

  /// Read-only extra folders the user added.
  final List<String> extraRoots;
  final List<WorkspaceUse> usedBy;

  /// The folder's own name: `name`, else the last path segment.
  String get label {
    if (name != null && name!.isNotEmpty) return name!;
    final parts = path.split('/').where((s) => s.isNotEmpty);
    return parts.isEmpty ? path : parts.last;
  }

  WorkspaceView copyWith({
    String? path,
    WorkspaceStatus? status,
    ShellBound? shellBound,
    List<String>? extraRoots,
    List<WorkspaceUse>? usedBy,
  }) => WorkspaceView(
    id: id,
    path: path ?? this.path,
    name: name,
    status: status ?? this.status,
    shellBound: shellBound ?? this.shellBound,
    extraRoots: extraRoots ?? this.extraRoots,
    usedBy: usedBy ?? this.usedBy,
  );
}

/// Something that uses a workspace, for Settings > Folders.
class WorkspaceUse {
  const WorkspaceUse(this.kind, this.id, this.label);
  final WorkspaceKind kind;
  final String id;
  final String label;

  WorkspaceTarget get target => WorkspaceTarget(kind, id);
}

/// The folder a target runs in, and why.
class WorkspaceResolution {
  const WorkspaceResolution(this.workspace, this.origin);
  final WorkspaceView workspace;
  final WorkspaceOrigin origin;
}

/// `validate_workspace`: the core's judgement of a path (section 5).
class Verdict {
  const Verdict({
    required this.canonical,
    this.errors = const [],
    this.warnings = const [],
    this.denied = false,
  });

  /// The real path after symlinks; may differ from what was chosen.
  final String canonical;

  /// Refusals, in the core's words. No override.
  final List<String> errors;
  final List<String> warnings;

  /// The daemon was refused by macOS (TCC), not by Tern's rules.
  final bool denied;

  bool get ok => errors.isEmpty && !denied;
}

/// A directory listing from a remote core (`GET /v1/workspaces/browse`).
class BrowseListing {
  const BrowseListing({required this.path, required this.dirs, this.parent});
  final String path;
  final String? parent;
  final List<String> dirs;
}

abstract class WorkspaceApi {
  /// True when the core runs on this computer, so a native picker can see its
  /// folders. False hides the picker and shows the core-side browser.
  bool get isLocal;

  /// The folder a run for [target] would use, and where it comes from.
  Future<WorkspaceResolution> resolve(WorkspaceTarget target);

  /// Bind [target] to [path]. Throws [WorkspaceRefused] with the core's
  /// reason.
  Future<WorkspaceResolution> set(WorkspaceTarget target, String path);

  /// Remove the binding so [target] inherits again.
  Future<WorkspaceResolution> clear(WorkspaceTarget target);

  /// The global default folder.
  Future<WorkspaceView> getDefault();
  Future<WorkspaceView> setDefault(String path);

  /// Every workspace, with what uses it.
  Future<List<WorkspaceView>> list();

  /// Stop tracking a workspace; what used it falls back to the default.
  Future<void> remove(String workspaceId);

  /// Point a missing workspace at a new place.
  Future<WorkspaceView> locate(String workspaceId, String path);

  /// Read-only extra folders of a workspace.
  Future<WorkspaceView> addRoot(String workspaceId, String path);
  Future<WorkspaceView> removeRoot(String workspaceId, String path);

  /// The core's verdict on [path], without binding anything.
  Future<Verdict> validate(String path);

  /// Remote cores only: directories below [path] (null = the core's home).
  Future<BrowseListing> browse(String? path);
}

/// Honest copy for a shell state (section 8). [folder] is the folder's name.
class ShellCopy {
  const ShellCopy(this.state, this.headline, this.detail, this.confined);
  final ShellBound state;

  /// Short label for the chip and tooltip.
  final String headline;

  /// The sentence the design prescribes for this state.
  final String detail;

  /// What is and is not confined, as plain lines.
  final List<String> confined;

  static ShellCopy of(ShellBound s, String folder) => switch (s) {
    ShellBound.bounded => ShellCopy(
      s,
      'Sandboxed',
      'Commands can change files only in $folder. They can still read some '
          'system files.',
      [
        'File tools: Tern confines them to this folder.',
        'Python: sees this folder, read only.',
        'Commands: can change files only here, but can read system files '
            '(on a Mac, anything outside your home folder).',
        'Connectors that run programs (MCP): not confined.',
      ],
    ),
    ShellBound.unverified => ShellCopy(
      s,
      'Container, not verified',
      'Commands run in a container that only sees $folder. Tern has not '
          'verified that.',
      [
        'File tools: Tern confines them to this folder.',
        'Python: sees this folder, read only.',
        'Commands: run in a container that mounts only this folder. Not '
            'proven by a check.',
        'Connectors that run programs (MCP): not confined.',
      ],
    ),
    ShellBound.unbounded => ShellCopy(
      s,
      'Commands not confined',
      "This Mac isn't sandboxing commands. A command can read or change "
          'anything you can. Tern asks before every one.',
      [
        'File tools: Tern confines them to this folder.',
        'Python: sees this folder, read only.',
        'Commands: NOT confined. The folder is only where they start.',
        'Connectors that run programs (MCP): not confined.',
        'Unattended runs never run commands on this machine.',
      ],
    ),
  };
}

/// Where a folder comes from, in words.
String originCopy(WorkspaceOrigin o) => switch (o) {
  WorkspaceOrigin.session => 'chosen for this topic',
  WorkspaceOrigin.channel => 'chosen for this channel',
  WorkspaceOrigin.agent => 'from the agent default',
  WorkspaceOrigin.global => 'from your default folder',
};
