// TernWorkspaceApi against a fake core over a unix socket: the request paths
// and bodies it sends, and how the replies map to what the UI shows.
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/workspaces.dart';
import 'package:tern_desktop/state/workspaces_adapter.dart';

Map<String, Object?> _ws(
  String id,
  String path, {
  String status = 'ok',
  String shell = 'unbounded',
  bool isDefault = false,
  List<Map<String, Object?>> roots = const [],
  Map<String, Object?> usedBy = const {},
}) => {
  'id': id,
  'name': path.split('/').last,
  'path': path,
  'is_default': isDefault,
  'status': status,
  'shell_state': shell,
  'roots': roots,
  'used_by': usedBy,
};

void main() {
  late Directory tmp;
  late HttpServer server;
  late TernWorkspaceApi api;
  final seen = <String>[];
  final bodies = <Object?>[];
  late (int, Object?) Function(String method, Uri uri) reply;

  setUp(() async {
    tmp = Directory.systemTemp.createTempSync('tern-wsa');
    final path = '${tmp.path}/s.sock';
    server = HttpServer.listenOn(
      await ServerSocket.bind(
        InternetAddress(path, type: InternetAddressType.unix),
        0,
      ),
    );
    server.listen((req) async {
      seen.add('${req.method} ${req.uri}');
      final text = await utf8.decodeStream(req);
      bodies.add(text.isEmpty ? null : jsonDecode(text));
      final (code, body) = reply(req.method, req.uri);
      req.response
        ..statusCode = code
        ..headers.contentType = ContentType.json
        ..write(jsonEncode(body));
      await req.response.close();
    });
    api = TernWorkspaceApi(TernClient(Endpoint.unix(path)), isLocal: true);
    seen.clear();
    bodies.clear();
  });

  tearDown(() async {
    await server.close(force: true);
    tmp.deleteSync(recursive: true);
  });

  Map<String, Object?> binding(String source, {bool own = true}) => {
    'workspace': own ? _ws('w2', '/p/app') : null,
    'effective': _ws('w2', '/p/app'),
    'source': source,
  };

  test('feature gate: only a core listing workspaces gets an API', () {
    expect(
      const Health(
        pid: 1,
        version: '0',
        features: {'workspaces'},
      ).hasWorkspaces,
      isTrue,
    );
    expect(const Health(pid: 1, version: '0').hasWorkspaces, isFalse);
  });

  test(
    'set and clear on session, channel and agent; source maps to origin',
    () async {
      reply = (m, u) =>
          (200, binding(u.path.contains('agents') ? 'agent' : 'session'));
      var r = await api.set(const WorkspaceTarget.session('s1'), '/p/app');
      expect(seen.last, 'PUT /v1/sessions/s1/workspace');
      expect(bodies.last, {'workspace': '/p/app'});
      expect(r.origin, WorkspaceOrigin.session);
      expect(r.workspace.path, '/p/app');

      reply = (m, u) => (200, binding('channel'));
      r = await api.set(const WorkspaceTarget.channel('c1'), '/p/app');
      expect(seen.last, 'PUT /v1/channels/c1/workspace');
      expect(r.origin, WorkspaceOrigin.channel);

      reply = (m, u) => (200, binding('default', own: false));
      r = await api.clear(const WorkspaceTarget.agent('a1'));
      expect(seen.last, 'PUT /v1/agents/a1/workspace');
      expect(bodies.last, {'workspace': null});
      expect(r.origin, WorkspaceOrigin.global);

      reply = (m, u) => (200, binding('agent'));
      r = await api.resolve(const WorkspaceTarget.agent('a1'));
      expect(seen.last, 'GET /v1/agents/a1/workspace');
      expect(r.origin, WorkspaceOrigin.agent);
    },
  );

  test(
    'a 422 with a verdict becomes WorkspaceRefused with the core wording',
    () async {
      reply = (m, u) => (
        422,
        {
          'error': 'That folder is too broad.',
          'verdict': {
            'errors': ['That folder is too broad.'],
          },
        },
      );
      await expectLater(
        api.set(const WorkspaceTarget.session('s1'), '/'),
        throwsA(
          isA<WorkspaceRefused>().having(
            (e) => e.message,
            'message',
            'That folder is too broad.',
          ),
        ),
      );
    },
  );

  test('shell_state and status map to the chip and card states', () async {
    reply = (m, u) => (
      200,
      [
        _ws('default', '/h', isDefault: true, shell: 'sandboxed'),
        _ws('a', '/a', shell: 'unverified'),
        _ws('b', '/b', shell: 'read_only', status: 'missing'),
        _ws('c', '/c', status: 'denied'),
        _ws('d', '/d', status: 'replaced'),
      ],
    );
    final all = await api.list();
    expect(seen.last, 'GET /v1/workspaces');
    expect(all.map((w) => w.shellBound), [
      ShellBound.bounded,
      ShellBound.unverified,
      ShellBound.bounded,
      ShellBound.unbounded,
      ShellBound.unbounded,
    ]);
    expect(all.map((w) => w.status), [
      WorkspaceStatus.ok,
      WorkspaceStatus.ok,
      WorkspaceStatus.missing,
      WorkspaceStatus.denied,
      WorkspaceStatus.replaced,
    ]);
    expect((await api.getDefault()).id, 'default');
  });

  test('used_by maps to what Settings > Folders lists', () async {
    reply = (m, u) => (
      200,
      [
        _ws(
          'a',
          '/a',
          usedBy: {
            'channels': ['ops'],
            'agents': ['mail'],
            'sessions': 2,
          },
        ),
      ],
    );
    final w = (await api.list()).single;
    expect(w.usedBy.map((u) => (u.kind, u.id)), [
      (WorkspaceKind.channel, 'ops'),
      (WorkspaceKind.agent, 'mail'),
    ]);
  });

  test('validate posts the path and returns the sheet text', () async {
    reply = (m, u) => (
      200,
      {
        'canonical': '/real/app',
        'warnings': ['It has many files.'],
        'suggested_root': '/real',
        'denied': false,
      },
    );
    final v = await api.validate('/link/app');
    expect(seen.last, 'POST /v1/workspaces/validate');
    expect(bodies.last, {'path': '/link/app'});
    expect(v.canonical, '/real/app');
    expect(v.ok, isTrue);
    expect(v.warnings.first, 'It has many files.');
    expect(v.warnings.last, contains('/real'));

    reply = (m, u) => (
      200,
      {
        'errors': ['Not a folder.'],
        'denied': true,
      },
    );
    final bad = await api.validate('/x');
    expect(bad.canonical, '/x');
    expect(bad.ok, isFalse);
    expect(bad.denied, isTrue);
    expect(bad.errors, ['Not a folder.']);
  });

  test('extra roots: add and remove hit the roots path, then reread', () async {
    reply = (m, u) {
      if (u.path.endsWith('/roots') && m == 'POST') {
        return (
          200,
          {
            'root': {'path': '/docs', 'kind': 'user'},
          },
        );
      }
      if (u.path.endsWith('/roots')) return (200, {});
      return (
        200,
        _ws(
          'w1',
          '/p',
          roots: [
            {'path': '/docs', 'kind': 'user'},
            {'path': '/p/.git', 'kind': 'git_dir'},
          ],
        ),
      );
    };
    final w = await api.addRoot('w1', '/docs');
    expect(seen.first, 'POST /v1/workspaces/w1/roots');
    expect(bodies.first, {'path': '/docs'});
    expect(seen.last, 'GET /v1/workspaces/w1');
    expect(w.extraRoots, ['/docs']);

    seen.clear();
    await api.removeRoot('w1', '/docs');
    expect(seen.first, 'DELETE /v1/workspaces/w1/roots?path=%2Fdocs');
  });

  test('remove reassigns to the default; locate confirms', () async {
    reply = (m, u) => (200, m == 'PATCH' ? _ws('w1', '/new') : {});
    await api.remove('w1');
    expect(seen.last, 'DELETE /v1/workspaces/w1?reassign_to=default');
    final w = await api.locate('w1', '/new');
    expect(seen.last, 'PATCH /v1/workspaces/w1');
    expect(bodies.last, {'locate': '/new', 'confirm': true});
    expect(w.path, '/new');
  });

  test('the default folder is not changeable from the app', () async {
    await expectLater(api.setDefault('/x'), throwsA(isA<WorkspaceRefused>()));
    expect(seen, isEmpty);
  });
}
