// Workspace models and client methods against a fake core (HTTPS with the
// self-signed fixture, as in remote_test.dart).
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

const _fixtures = 'test/fixtures';

List<int> _der(String pem) {
  final body = pem
      .split('\n')
      .skipWhile((l) => !l.startsWith('-----BEGIN CERTIFICATE'))
      .skip(1)
      .takeWhile((l) => !l.startsWith('-----END'))
      .join();
  return base64.decode(body);
}

class _Seen {
  _Seen(this.method, this.path, this.body);
  final String method;
  final String path;
  final Map<String, dynamic>? body;
}

class _Core {
  _Core(this.server);
  final HttpServer server;
  final seen = <_Seen>[];
  int status = 200;
  Object reply = <String, Object?>{};

  static Future<_Core> start() async {
    final ctx = SecurityContext()
      ..useCertificateChain('$_fixtures/cert.pem')
      ..usePrivateKey('$_fixtures/key.pem');
    final core = _Core(await HttpServer.bindSecure('127.0.0.1', 0, ctx));
    core.server.listen((req) async {
      final text = await utf8.decoder.bind(req).join();
      core.seen.add(
        _Seen(
          req.method,
          req.uri.toString(),
          text.isEmpty ? null : jsonDecode(text) as Map<String, dynamic>,
        ),
      );
      req.response
        ..statusCode = core.status
        ..headers.contentType = ContentType.json
        ..write(jsonEncode(core.reply));
      await req.response.close();
    });
    return core;
  }
}

Map<String, Object?> _ws(String id, {String path = '/p', String? status}) => {
  'id': id,
  'name': id,
  'path': path,
  'is_default': id == 'default',
  'status': status ?? 'ok',
  'created_by': 'user',
  'archived': false,
  'writable': true,
  'flags': {'case_insensitive': false, 'normalizes_unicode': false},
  'shell_state': 'sandboxed',
  'roots': [
    {'path': '/docs', 'kind': 'user', 'is_file': false},
    {'path': '/docs/a.md', 'kind': 'user', 'is_file': true},
  ],
  'used_by': {
    'channels': ['launch'],
    'sessions': 2,
    'agents': <String>[],
    'routines': <String>[],
  },
};

void main() {
  late _Core core;
  late TernClient client;

  setUp(() async {
    core = await _Core.start();
    final pin = fingerprint(
      _der(File('$_fixtures/cert.pem').readAsStringSync()),
    );
    client = TernClient(
      RemoteEndpoint(
        Uri.parse('https://127.0.0.1:${core.server.port}'),
        token: 't',
        pin: pin,
      ),
    );
  });
  tearDown(() async {
    client.close();
    await core.server.close(force: true);
  });

  test('a workspace decodes, with its shell state, roots and users', () {
    final w = Workspace.fromJson(_ws('ws_1', status: 'missing'));
    expect(
      (w.isOk, w.isDefault, w.shellState),
      (false, false, ShellState.sandboxed),
    );
    expect(w.roots.map((r) => (r.path, r.isFile)), [
      ('/docs', false),
      ('/docs/a.md', true),
    ]);
    expect(w.usedBy.channels, ['launch']);
    expect((w.usedBy.sessions, w.usedBy.isEmpty), (2, false));
    for (final (wire, state) in [
      ('sandboxed', ShellState.sandboxed),
      ('read_only', ShellState.readOnly),
      ('unverified', ShellState.unverified),
      ('unbounded', ShellState.unbounded),
      ('something_new', ShellState.unbounded),
    ]) {
      expect(ShellState.parse(wire), state, reason: wire);
    }
    final v = WorkspaceVerdict.fromJson({
      'canonical': '/real',
      'shortcut_of': '/link',
      'kind': 'dir',
      'errors': <String>[],
      'warnings': ['inside a repo'],
      'suggested_root': '/repo',
      'denied': false,
    });
    expect((v.isOk, v.shortcutOf, v.suggestedRoot), (true, '/link', '/repo'));
    expect(v.warnings, ['inside a repo']);
    expect(
      WorkspaceVerdict.fromJson({
        'errors': ['no'],
        'kind': 'dir',
      }).isOk,
      isFalse,
    );
  });

  test('health says whether the core has workspaces', () async {
    core.reply = {
      'pid': 1,
      'version': '0.1.0',
      'features': ['channels', 'workspaces'],
    };
    expect((await client.health()).hasWorkspaces, isTrue);
    core.reply = {
      'pid': 1,
      'version': '0.1.0',
      'features': ['channels'],
    };
    expect((await client.health()).hasWorkspaces, isFalse);
  });

  test('list, get and validate', () async {
    core.reply = [_ws('default'), _ws('ws_1')];
    final all = await client.workspaces();
    expect(all.map((w) => w.id), ['default', 'ws_1']);
    expect(core.seen.last.path, '/v1/workspaces');
    await client.workspaces(archived: true);
    expect(core.seen.last.path, '/v1/workspaces?archived=1');
    core.reply = _ws('ws_1');
    expect((await client.workspace('ws_1')).id, 'ws_1');
    core.reply = {
      'canonical': '/x',
      'kind': 'dir',
      'errors': <String>[],
      'warnings': <String>[],
    };
    final v = await client.validateWorkspace('/x', readOnly: true);
    expect(v.isOk, isTrue);
    expect(core.seen.last.method, 'POST');
    expect(core.seen.last.path, '/v1/workspaces/validate');
    expect(core.seen.last.body, {'path': '/x', 'kind': 'read_root'});
  });

  test(
    'a refused folder throws with the verdict; a good one returns',
    () async {
      core
        ..status = 422
        ..reply = {
          'error': "That's the whole disk.",
          'verdict': {
            'kind': 'dir',
            'errors': ["That's the whole disk."],
            'warnings': <String>[],
          },
        };
      await expectLater(
        client.addWorkspace('/'),
        throwsA(
          isA<WorkspaceRefused>()
              .having((e) => e.message, 'message', contains('whole disk'))
              .having((e) => e.verdict.errors, 'errors', hasLength(1)),
        ),
      );
      core
        ..status = 201
        ..reply = {
          'workspace': _ws('ws_2', path: '/proj'),
          'verdict': {
            'kind': 'dir',
            'errors': <String>[],
            'warnings': <String>[],
          },
        };
      final w = await client.addWorkspace('/proj', name: 'Proj');
      expect(w.path, '/proj');
      expect(core.seen.last.body, {'path': '/proj', 'name': 'Proj'});
    },
  );

  test('send and channel create carry the folder', () async {
    core.reply = {
      'post': null,
      'run_id': 'r',
      'session_id': 's',
      'queued': false,
      'new_segment': true,
    };
    final sent = await client.sendToChannel(
      'home',
      'hi',
      newTopic: true,
      workspace: '/proj',
    );
    expect(sent.newSegment, isTrue);
    expect(core.seen.last.path, '/v1/channels/home/posts');
    expect(core.seen.last.body, {
      'text': 'hi',
      'new_topic': true,
      'workspace': '/proj',
    });
    await client.sendToChannel('home', 'plain');
    expect(core.seen.last.body, {'text': 'plain'}, reason: 'no folder, no key');

    core.reply = {'session_id': 's', 'run_id': 'r', 'model': 'm'};
    await client.send('x', workspace: 'ws_1');
    expect(core.seen.last.body, {'text': 'x', 'workspace': 'ws_1'});

    core
      ..status = 201
      ..reply = {
        'id': 'c',
        'slug': 'launch',
        'title': 'launch',
        'kind': 'topic',
        'needs_you': 0,
        'active_runs': 0,
        'updated_at': 1,
      };
    await client.createChannel(slug: 'launch', workspace: '/proj');
    expect(core.seen.last.body?['workspace'], '/proj');
    expect(core.seen.last.body?['slug'], 'launch');
  });

  test('bindings: get, set, clear for channel, session and agent', () async {
    core.reply = {
      'workspace': null,
      'effective': _ws('default'),
      'source': 'default',
    };
    final b = await client.channelWorkspace('launch');
    expect(
      (b.workspace, b.effective.isDefault, b.source),
      (null, true, 'default'),
    );
    expect(core.seen.last.path, '/v1/channels/launch/workspace');

    core.reply = {
      'workspace': _ws('ws_1'),
      'effective': _ws('ws_1'),
      'source': 'channel',
    };
    final set = await client.setChannelWorkspace('launch', 'ws_1');
    expect(set.workspace?.id, 'ws_1');
    expect(core.seen.last.method, 'PUT');
    expect(core.seen.last.body, {'workspace': 'ws_1'});

    await client.setSessionWorkspace('sess', '/proj');
    expect(core.seen.last.path, '/v1/sessions/sess/workspace');
    expect(core.seen.last.body, {'workspace': '/proj'});

    await client.setAgentWorkspace('tern:research', null);
    expect(core.seen.last.path, '/v1/agents/tern%3Aresearch/workspace');
    expect(core.seen.last.body, {'workspace': null}, reason: 'null clears');

    await client.agentWorkspace('tern');
    await client.sessionWorkspace('sess');
    expect(core.seen.last.method, 'GET');

    core
      ..status = 422
      ..reply = {
        'error': 'no',
        'verdict': {
          'kind': 'dir',
          'errors': ['no'],
          'warnings': <String>[],
        },
      };
    await expectLater(
      client.setChannelWorkspace('launch', '/etc'),
      throwsA(isA<WorkspaceRefused>()),
    );
  });

  test('roots, update, locate, check and remove', () async {
    core
      ..status = 201
      ..reply = {
        'root': {'path': '/docs', 'kind': 'user', 'is_file': false},
        'verdict': {
          'kind': 'dir',
          'errors': <String>[],
          'warnings': <String>[],
        },
      };
    final r = await client.addWorkspaceRoot('ws_1', '/docs');
    expect((r.path, r.isFile), ('/docs', false));
    core
      ..status = 200
      ..reply = [
        {'path': '/docs', 'kind': 'user', 'is_file': false},
      ];
    expect((await client.workspaceRoots('ws_1')).single.path, '/docs');
    core.reply = {'ok': true};
    await client.removeWorkspaceRoot('ws_1', '/my docs');
    expect(core.seen.last.method, 'DELETE');
    expect(core.seen.last.path, '/v1/workspaces/ws_1/roots?path=%2Fmy+docs');

    core.reply = _ws('ws_1');
    await client.updateWorkspace('ws_1', name: 'New', network: 'off');
    expect(core.seen.last.body, {'name': 'New', 'network': 'off'});
    await client.updateWorkspace('ws_1', clearNetwork: true);
    expect(core.seen.last.body, {'network': null});
    await client.locateWorkspace('ws_1', '/new', confirm: true);
    expect(core.seen.last.body, {'locate': '/new', 'confirm': true});
    await client.checkWorkspace('ws_1');
    expect(
      (core.seen.last.method, core.seen.last.path),
      ('POST', '/v1/workspaces/ws_1/check'),
    );

    core.reply = {'ok': true};
    await client.removeWorkspace('ws_1', reassignTo: 'default');
    expect(core.seen.last.path, '/v1/workspaces/ws_1?reassign_to=default');
  });

  test('errors keep their JSON body (needs_confirm, impact)', () async {
    core
      ..status = 409
      ..reply = {
        'error': 'this folder is in use; pass reassign_to to move what uses it',
        'impact': {
          'channels': ['launch'],
          'sessions': 0,
          'agents': <String>[],
          'routines': <String>[],
        },
      };
    await expectLater(
      client.removeWorkspace('ws_1'),
      throwsA(
        isA<TernException>().having((e) => e.status, 'status', 409).having(
          (e) => WorkspaceUsedBy.fromJson(
            e.body!['impact'] as Map<String, dynamic>,
          ).channels,
          'impact channels',
          ['launch'],
        ),
      ),
    );
    core.reply = {
      'error': "this isn't the folder it was",
      'needs_confirm': true,
    };
    await expectLater(
      client.locateWorkspace('ws_1', '/other'),
      throwsA(
        isA<TernException>().having(
          (e) => e.body?['needs_confirm'],
          'needs_confirm',
          true,
        ),
      ),
    );
  });
}
