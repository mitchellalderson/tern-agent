// The ONE file that connects the desktop's [WorkspaceApi] to the core
// (docs/alpha-workspaces.md), through tern_client's workspace methods. The
// UI works on [WorkspaceView]s; this file maps tern_client's `Workspace`,
// `WorkspaceBinding` and `WorkspaceVerdict` to them.
//
// The folder UI shows only when the core lists the `workspaces` feature, so
// an older core behaves exactly as before.
import 'package:tern_client/tern_client.dart' as tc;
import 'package:tern_client/tern_client.dart' show RemoteEndpoint;

import 'store.dart';
import 'workspaces.dart';

/// The adapter is real.
const kWorkspacesWired = true;

/// The core's feature flag (`/v1/health` features) for workspaces.
const workspacesFeature = 'workspaces';

/// The API for [store]'s core, or null when the folder UI must stay hidden:
/// the core does not list the `workspaces` feature.
WorkspaceApi? workspaceApiFor(TernStore store) {
  if (!kWorkspacesWired) return null;
  if (!(store.health?.hasWorkspaces ?? false)) return null;
  return TernWorkspaceApi(
    store.client,
    isLocal: store.client.endpoint is! RemoteEndpoint,
  );
}

class TernWorkspaceApi implements WorkspaceApi {
  TernWorkspaceApi(this.client, {required this.isLocal});
  final tc.TernClient client;

  @override
  final bool isLocal;

  // ---- mapping ----

  static WorkspaceStatus _status(String s) => switch (s) {
    'replaced' => WorkspaceStatus.replaced,
    'denied' => WorkspaceStatus.denied,
    'missing' || 'invalid' => WorkspaceStatus.missing,
    _ => WorkspaceStatus.ok,
  };

  static ShellBound _shell(tc.ShellState s) => switch (s) {
    tc.ShellState.sandboxed || tc.ShellState.readOnly => ShellBound.bounded,
    tc.ShellState.unverified => ShellBound.unverified,
    tc.ShellState.unbounded => ShellBound.unbounded,
  };

  static WorkspaceOrigin _origin(String s) => switch (s) {
    'session' => WorkspaceOrigin.session,
    'channel' => WorkspaceOrigin.channel,
    'agent' => WorkspaceOrigin.agent,
    _ => WorkspaceOrigin.global,
  };

  static WorkspaceView view(tc.Workspace w) => WorkspaceView(
    id: w.id,
    path: w.path,
    name: w.name,
    status: _status(w.status),
    shellBound: _shell(w.shellState),
    extraRoots: [
      for (final r in w.roots)
        if (r.kind == 'user') r.path,
    ],
    usedBy: [
      for (final c in w.usedBy.channels)
        WorkspaceUse(WorkspaceKind.channel, c, c),
      for (final a in w.usedBy.agents) WorkspaceUse(WorkspaceKind.agent, a, a),
    ],
  );

  static WorkspaceResolution _resolution(tc.WorkspaceBinding b) =>
      WorkspaceResolution(view(b.effective), _origin(b.source));

  /// The words for the confirm sheet.
  static Verdict verdict(tc.WorkspaceVerdict v, String chosen) => Verdict(
    canonical: v.canonical ?? chosen,
    errors: v.errors,
    warnings: [
      ...v.warnings,
      if (v.suggestedRoot case final r?)
        'This folder is inside a repository. Its folder is $r.',
    ],
    denied: v.denied,
  );

  // ---- bindings ----

  @override
  Future<WorkspaceResolution> resolve(WorkspaceTarget target) async =>
      _resolution(switch (target.kind) {
        WorkspaceKind.session => await client.sessionWorkspace(target.id),
        WorkspaceKind.channel => await client.channelWorkspace(target.id),
        WorkspaceKind.agent => await client.agentWorkspace(target.id),
      });

  Future<WorkspaceResolution> _bind(WorkspaceTarget t, String? path) async =>
      _resolution(switch (t.kind) {
        WorkspaceKind.session => await client.setSessionWorkspace(t.id, path),
        WorkspaceKind.channel => await client.setChannelWorkspace(t.id, path),
        WorkspaceKind.agent => await client.setAgentWorkspace(t.id, path),
      });

  @override
  Future<WorkspaceResolution> set(WorkspaceTarget t, String path) =>
      _bind(t, path);

  @override
  Future<WorkspaceResolution> clear(WorkspaceTarget target) =>
      _bind(target, null);

  // ---- the list ----

  @override
  Future<WorkspaceView> getDefault() async {
    final all = await client.workspaces();
    return view(all.firstWhere((w) => w.isDefault, orElse: () => all.first));
  }

  @override
  Future<WorkspaceView> setDefault(String path) async => throw refusal(
    'The default folder is set in config.toml (workspace = ...); '
    'it cannot be changed from the app.',
  );

  @override
  Future<List<WorkspaceView>> list() async => [
    for (final w in await client.workspaces()) view(w),
  ];

  @override
  Future<void> remove(String workspaceId) =>
      client.removeWorkspace(workspaceId, reassignTo: 'default');

  @override
  Future<WorkspaceView> locate(String workspaceId, String path) async =>
      view(await client.locateWorkspace(workspaceId, path, confirm: true));

  // ---- read-only extra folders ----

  @override
  Future<WorkspaceView> addRoot(String workspaceId, String path) async {
    await client.addWorkspaceRoot(workspaceId, path);
    return view(await client.workspace(workspaceId));
  }

  @override
  Future<WorkspaceView> removeRoot(String workspaceId, String path) async {
    await client.removeWorkspaceRoot(workspaceId, path);
    return view(await client.workspace(workspaceId));
  }

  @override
  Future<Verdict> validate(String path) async =>
      verdict(await client.validateWorkspace(path), path);

  @override
  Future<BrowseListing> browse(String? path) async =>
      throw refusal('This core cannot list its folders yet.');
}
