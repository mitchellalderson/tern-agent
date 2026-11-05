import 'package:tern_desktop/state/workspaces.dart';

/// In-memory [WorkspaceApi] for widget tests.
class FakeWorkspaces implements WorkspaceApi {
  FakeWorkspaces({
    this.isLocal = true,
    this.shell = ShellBound.unbounded,
    String defaultPath = '/Users/me/tern-workspace',
  }) {
    _default = _make(defaultPath);
  }

  @override
  final bool isLocal;
  ShellBound shell;

  late WorkspaceView _default;
  final all = <WorkspaceView>[];
  final bindings = <WorkspaceTarget, String>{}; // target -> workspace id
  final calls = <String>[];

  /// Paths the fake refuses, with the core's reason.
  final refuse = <String, String>{
    '/': 'This folder is not allowed: it is the root of the disk.',
    '/Users/me': 'This folder is not allowed: it is your home folder.',
    '/System': 'This folder is not allowed: it is a system folder.',
    '/Users/me/.tern':
        "This folder is not allowed: it overlaps Tern's own data.",
  };
  final warn = <String, List<String>>{};
  final deniedPaths = <String>{};

  /// Remote tree for [browse].
  final tree = <String, List<String>>{
    '/home/me': ['projects', 'notes'],
    '/home/me/projects': ['site', 'api'],
  };

  int _n = 0;
  WorkspaceView _make(String path) {
    final w = WorkspaceView(id: 'ws${_n++}', path: path, shellBound: shell);
    all.add(w);
    return w;
  }

  void _replace(WorkspaceView w) {
    final i = all.indexWhere((x) => x.id == w.id);
    if (i >= 0) all[i] = w;
    if (_default.id == w.id) _default = w;
  }

  WorkspaceView _byId(String id) => all.firstWhere((w) => w.id == id);

  WorkspaceView _forPath(String path) {
    for (final w in all) {
      if (w.path == path) return w;
    }
    return _make(path);
  }

  /// Put a workspace in a state, for the cards.
  void markStatus(String path, WorkspaceStatus s) =>
      _replace(_forPath(path).copyWith(status: s));

  @override
  Future<WorkspaceResolution> resolve(WorkspaceTarget t) async {
    calls.add('resolve ${t.kind.name}:${t.id}');
    final own = bindings[t];
    if (own != null) {
      return WorkspaceResolution(_byId(own), switch (t.kind) {
        WorkspaceKind.session => WorkspaceOrigin.session,
        WorkspaceKind.channel => WorkspaceOrigin.channel,
        WorkspaceKind.agent => WorkspaceOrigin.agent,
      });
    }
    return WorkspaceResolution(_default, WorkspaceOrigin.global);
  }

  Future<void> _check(String path) async {
    final v = await validate(path);
    if (v.errors.isNotEmpty) throw refusal(v.errors.first);
    if (v.denied) {
      throw refusal('macOS did not let Tern open it.');
    }
  }

  @override
  Future<WorkspaceResolution> set(WorkspaceTarget t, String path) async {
    calls.add('set ${t.kind.name}:${t.id} $path');
    await _check(path);
    bindings[t] = _forPath(path).id;
    return resolve(t);
  }

  @override
  Future<WorkspaceResolution> clear(WorkspaceTarget t) async {
    calls.add('clear ${t.kind.name}:${t.id}');
    bindings.remove(t);
    return resolve(t);
  }

  @override
  Future<WorkspaceView> getDefault() async => _default;

  @override
  Future<WorkspaceView> setDefault(String path) async {
    calls.add('setDefault $path');
    await _check(path);
    _default = _forPath(path);
    return _default;
  }

  @override
  Future<List<WorkspaceView>> list() async => [
    for (final w in all)
      w.copyWith(
        usedBy: [
          for (final e in bindings.entries)
            if (e.value == w.id)
              WorkspaceUse(
                e.key.kind,
                e.key.id,
                '${e.key.id} ${e.key.kind.name}',
              ),
        ],
      ),
  ];

  @override
  Future<void> remove(String id) async {
    calls.add('remove $id');
    bindings.removeWhere((_, v) => v == id);
    all.removeWhere((w) => w.id == id && w.id != _default.id);
  }

  @override
  Future<WorkspaceView> locate(String id, String path) async {
    calls.add('locate $id $path');
    await _check(path);
    final w = _byId(id).copyWith(path: path, status: WorkspaceStatus.ok);
    _replace(w);
    return w;
  }

  @override
  Future<WorkspaceView> addRoot(String id, String path) async {
    calls.add('addRoot $id $path');
    final w = _byId(id);
    final n = w.copyWith(extraRoots: [...w.extraRoots, path]);
    _replace(n);
    return n;
  }

  @override
  Future<WorkspaceView> removeRoot(String id, String path) async {
    calls.add('removeRoot $id $path');
    final w = _byId(id);
    final n = w.copyWith(
      extraRoots: [
        for (final r in w.extraRoots)
          if (r != path) r,
      ],
    );
    _replace(n);
    return n;
  }

  @override
  Future<Verdict> validate(String path) async {
    calls.add('validate $path');
    return Verdict(
      canonical: path,
      errors: [?refuse[path]],
      warnings: warn[path] ?? const [],
      denied: deniedPaths.contains(path),
    );
  }

  @override
  Future<BrowseListing> browse(String? path) async {
    final p = path ?? '/home/me';
    final parent = p == '/home/me'
        ? null
        : (p.split('/')..removeLast()).join('/');
    return BrowseListing(path: p, parent: parent, dirs: tree[p] ?? const []);
  }
}
