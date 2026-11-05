// The ONE file that connects the desktop's [WorkspaceApi] to the core.
//
// TODO(workspaces-wire): the core API (docs/alpha-workspaces.md W0/W1) has not
// merged. When it has, implement each method below with the matching
// tern_client calls, flip [kWorkspacesWired] to true, and delete this note.
// Until then the app asks for the adapter only through [workspaceApiFor],
// which returns null, so every folder control is hidden and an older core
// without the endpoint behaves exactly as before.
import 'package:tern_client/tern_client.dart';

import 'store.dart';
import 'workspaces.dart';

/// False until the methods below are real.
const kWorkspacesWired = false;

/// The core's feature flag (`/v1/health` features) for workspaces.
const workspacesFeature = 'workspaces';

/// The API for [store]'s core, or null when the folder UI must stay hidden:
/// not wired yet, or the core does not list the `workspaces` feature.
WorkspaceApi? workspaceApiFor(TernStore store) {
  if (!kWorkspacesWired) return null;
  if (!(store.health?.features.contains(workspacesFeature) ?? false)) {
    return null;
  }
  return TernWorkspaceApi(store);
}

class TernWorkspaceApi implements WorkspaceApi {
  TernWorkspaceApi(this.store);
  final TernStore store;

  static Never _todo() =>
      throw UnimplementedError('workspaces: wire to tern_client (see TODO)');

  @override
  bool get isLocal => store.client.endpoint is! RemoteEndpoint;

  @override
  Future<WorkspaceResolution> resolve(WorkspaceTarget target) async => _todo();
  @override
  Future<WorkspaceResolution> set(WorkspaceTarget t, String path) async =>
      _todo();
  @override
  Future<WorkspaceResolution> clear(WorkspaceTarget target) async => _todo();
  @override
  Future<Workspace> getDefault() async => _todo();
  @override
  Future<Workspace> setDefault(String path) async => _todo();
  @override
  Future<List<Workspace>> list() async => _todo();
  @override
  Future<void> remove(String workspaceId) async => _todo();
  @override
  Future<Workspace> locate(String workspaceId, String path) async => _todo();
  @override
  Future<Workspace> addRoot(String workspaceId, String path) async => _todo();
  @override
  Future<Workspace> removeRoot(String workspaceId, String path) async =>
      _todo();
  @override
  Future<Verdict> validate(String path) async => _todo();
  @override
  Future<BrowseListing> browse(String? path) async => _todo();
}
