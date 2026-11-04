import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

Json j(String s) => jsonDecode(s) as Json;

const _status =
    '{"name":"github","config":{"transport":"stdio","command":"gh-mcp",'
    '"args":["stdio"],"env":{"LOG_LEVEL":"warn"},"env_secrets":["GITHUB_TOKEN"],'
    '"disabled_tools":["delete_repo"],"effects":{"*":"write"}},'
    '"state":{"kind":"running","tools":2,"since":1762000000},'
    '"tools":[{"exposed":"mcp__github__search","remote":"search",'
    '"description":"Search","disabled":false},{"exposed":'
    '"mcp__github__delete_repo","remote":"delete_repo","description":"",'
    '"disabled":true}],"missing_secrets":[],"server_info":"gh 0.4.1",'
    '"stderr_tail":["ready"]}';

void main() {
  test('decodes statuses and every state', () {
    final s = McpStatus.fromJson(j(_status));
    expect(s.config.command, 'gh-mcp');
    expect(s.config.env, {'LOG_LEVEL': 'warn'});
    expect(s.config.secretNames, ['GITHUB_TOKEN']);
    expect(s.config.effects, {'*': 'write'});
    final r = s.state as McpRunning;
    expect(r.tools, 2);
    expect(r.since.millisecondsSinceEpoch, 1762000000000);
    expect(s.tools.last.disabled, isTrue);
    expect(s.serverInfo, 'gh 0.4.1');

    final u = McpState.fromJson(
      j(
        '{"kind":"unavailable","error":"exited","retry_at":1762000012,'
        '"failures":3}',
      ),
    );
    expect((u as McpUnavailable).failures, 3);
    expect(u.retryAt, isNotNull);
    expect(
      (McpState.fromJson(
                j(
                  '{"kind":"unavailable","error":"x","retry_at":null,"failures":1}',
                ),
              )
              as McpUnavailable)
          .retryAt,
      isNull,
    );
    expect(McpState.fromJson(j('{"kind":"disabled"}')), isA<McpDisabled>());
    expect(McpState.fromJson(j('{"kind":"connecting"}')), isA<McpConnecting>());
    expect(McpState.fromJson(j('{"kind":"new"}')), isA<McpNotStarted>());
  });

  test('configs leave out defaults and the other transport', () {
    expect(const McpServerConfig(command: 'x').toJson(), {
      'transport': 'stdio',
      'command': 'x',
    });
    expect(
      const McpServerConfig(
        transport: McpTransport.http,
        enabled: false,
        command: 'stale',
        url: 'https://x',
        bearerTokenSecret: 'X_TOKEN',
        headerSecrets: {'X-Api-Key': 'X_KEY'},
        disabledTools: ['a'],
        requiresApproval: true,
      ).toJson(),
      {
        'transport': 'http',
        'enabled': false,
        'url': 'https://x',
        'bearer_token_secret': 'X_TOKEN',
        'header_secrets': {'X-Api-Key': 'X_KEY'},
        'disabled_tools': ['a'],
        'requires_approval': true,
      },
    );
    final round = McpServerConfig.fromJson(
      McpStatus.fromJson(j(_status)).config.toJson(),
    );
    expect(round.args, ['stdio']);
    expect(round.disabledTools, ['delete_repo']);
  });

  test('decodes verify results, imports and mcp_changed', () {
    final ok = McpVerifyResult.fromJson(
      j(
        '{"ok":true,"verified":{"tools":[{"exposed":"mcp__a__b","remote":"b",'
        '"description":"","disabled":false}],"server_info":"a 1.0"}}',
      ),
    );
    expect(ok.ok, isTrue);
    expect(ok.verified!.tools.single.remote, 'b');
    final bad = McpVerifyResult.fromJson(
      j(
        '{"ok":false,"error":{"kind":"exited_early","message":"closed",'
        '"stderr_tail":["Error: bad token"]}}',
      ),
    );
    expect(bad.ok, isFalse);
    expect(bad.error!.label, 'Exited during startup');
    expect(bad.error!.stderrTail, ['Error: bad token']);

    final i = ImportedServer.fromJson(
      j(
        '{"name":"github","config":{"transport":"stdio","command":"npx",'
        '"env_secrets":["GITHUB_TOKEN","OTHER"]},'
        '"secrets":{"GITHUB_TOKEN":"ghp_x","OTHER":null},"warnings":["w"]}',
      ),
    );
    expect(i.secrets, {'GITHUB_TOKEN': 'ghp_x', 'OTHER': null});
    expect(i.warnings, ['w']);

    final e = TernEvent.fromJson(
      j('{"mcp_changed":{"name":"github","state":{"kind":"connecting"}}}'),
    );
    expect(e, isA<McpChanged>().having((e) => e.name, 'name', 'github'));
    expect((e as McpChanged).state, isA<McpConnecting>());
    final gone = TernEvent.fromJson(
      j('{"mcp_changed":{"name":"github","state":null}}'),
    );
    expect((gone as McpChanged).state, isNull);
  });

  group('over HTTP', () {
    late HttpServer server;
    late TernClient client;
    final seen = <String>[];
    var status = 200;
    var answer = _status;

    setUp(() async {
      seen.clear();
      status = 200;
      answer = _status;
      server = await HttpServer.bind('127.0.0.1', 0);
      server.listen((req) async {
        final body = await utf8.decoder.bind(req).join();
        seen.add('${req.method} ${req.uri} $body'.trim());
        req.response
          ..statusCode = status
          ..headers.contentType = ContentType.json
          ..write(answer);
        await req.response.close();
      });
      client = TernClient(
        Endpoint.remote(
          Uri.parse('http://127.0.0.1:${server.port}'),
          token: 't',
        ),
      );
    });
    tearDown(() async {
      client.close();
      await server.close(force: true);
    });

    test('lists and reads servers', () async {
      answer = '[$_status]';
      expect((await client.mcpServers()).single.name, 'github');
      answer = _status;
      expect((await client.mcpServer('github')).stderrTail, ['ready']);
      expect(seen, ['GET /v1/mcp', 'GET /v1/mcp/github']);
    });

    test('verifies with typed secrets', () async {
      answer =
          '{"ok":false,"error":{"kind":"unauthorized","message":"401",'
          '"stderr_tail":[]}}';
      final r = await client.verifyMcp(
        'github',
        const McpServerConfig(command: 'gh', envSecrets: ['GITHUB_TOKEN']),
        secrets: {'GITHUB_TOKEN': 'v'},
      );
      expect(r.error?.kind, 'unauthorized');
      expect(
        seen.single,
        'POST /v1/mcp/verify {"name":"github","config":{"transport":"stdio",'
        '"command":"gh","env_secrets":["GITHUB_TOKEN"]},'
        '"secrets":{"GITHUB_TOKEN":"v"}}',
      );
    });

    test('imports, adds, updates, patches, restarts and removes', () async {
      answer =
          '[{"name":"a","config":{"transport":"http","url":"https://a"},'
          '"secrets":{},"warnings":[]}]';
      expect((await client.importMcp('{}')).single.config.url, 'https://a');
      answer = _status;
      status = 201;
      await client.addMcp('github', const McpServerConfig(command: 'gh'));
      status = 200;
      await client.updateMcp(
        'github',
        name: 'gh',
        config: const McpServerConfig(command: 'gh'),
        secrets: {'T': 'v'},
      );
      await client.patchMcp('gh', disabledTools: ['x'], enabled: false);
      await client.restartMcp('gh');
      answer = '{"ok":true}';
      await client.removeMcp('gh', removeSecrets: true);
      expect(seen, [
        'POST /v1/mcp/import {"text":"{}"}',
        'POST /v1/mcp {"name":"github","config":{"transport":"stdio",'
            '"command":"gh"}}',
        'PUT /v1/mcp/github {"name":"gh","config":{"transport":"stdio",'
            '"command":"gh"},"secrets":{"T":"v"}}',
        'PATCH /v1/mcp/gh {"enabled":false,"disabled_tools":["x"]}',
        'POST /v1/mcp/gh/restart',
        'DELETE /v1/mcp/gh?remove_secrets=true',
      ]);
    });

    test('a taken name and a refused stdio add surface their status', () async {
      status = 409;
      answer = '{"error":"MCP server github already exists"}';
      await expectLater(
        client.addMcp('github', const McpServerConfig(command: 'gh')),
        throwsA(isA<TernException>().having((e) => e.status, 'status', 409)),
      );
      status = 403;
      answer = '{"error":"adding local commands from another device is off"}';
      await expectLater(
        client.verifyMcp('x', const McpServerConfig(command: 'gh')),
        throwsA(isA<TernException>().having((e) => e.status, 'status', 403)),
      );
    });
  });
}
