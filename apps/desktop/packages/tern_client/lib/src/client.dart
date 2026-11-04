import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'agents.dart';
import 'bundles.dart';
import 'channels.dart';
import 'config.dart';
import 'connections.dart';
import 'endpoint.dart';
import 'ledger.dart';
import 'lifecycle.dart';
import 'mcp.dart';
import 'outbox.dart';
import 'models.dart';
import 'providers.dart';
import 'secret_requests.dart';
import 'sse.dart';
import 'subagents.dart';
import 'security.dart';
import 'tasks.dart';
import 'tools.dart';
import 'webhooks.dart';

/// An error response from the daemon (`{"error": "..."}`) or a failure to
/// reach it.
class TernException implements Exception {
  const TernException(this.message, {this.status});

  final String message;
  final int? status;

  bool get unreachable => status == null;

  @override
  String toString() => message;
}

/// HTTP 401 from a remote core: the device token was revoked or never
/// valid. Show "Signed out of <core>" with re-pair; retrying won't help.
class SignedOutException extends TernException {
  const SignedOutException(super.message, {required this.core})
    : super(status: 401);

  final String core;
}

/// What redeeming a pairing link returns.
class Pairing {
  const Pairing({
    required this.deviceId,
    required this.scope,
    required this.endpoint,
  });

  final String deviceId;

  /// `owner`, `chat` or `read`.
  final String scope;

  /// Ready to use; keep its token in the Keychain, not in preferences.
  final RemoteEndpoint endpoint;
}

class TernClient {
  TernClient(this.endpoint) : _http = endpoint.httpClient();

  final Endpoint endpoint;
  final HttpClient _http;

  void close() => _http.close(force: true);

  /// Redeem a pairing link (deployment spec §6.2 step 2), pinning the
  /// core's certificate when the link has a fingerprint.
  static Future<Pairing> pair(
    PairingLink link, {
    required String deviceName,
    String? platform,
  }) async {
    // A throwaway token: pairing is the one route that needs none.
    final bootstrap = RemoteEndpoint(
      link.url,
      token: '',
      pin: link.fingerprint,
    );
    final client = TernClient(bootstrap);
    try {
      final j =
          await client._request('POST', '/v1/auth/pair', {
                'code': link.code,
                'device_name': deviceName,
                'platform': ?platform,
              }, auth: false)
              as Json;
      return Pairing(
        deviceId: j['device_id'] as String,
        scope: j['scope'] as String,
        endpoint: RemoteEndpoint(
          link.url,
          token: j['token'] as String,
          pin: link.fingerprint,
        ),
      );
    } on SignedOutException catch (e) {
      // On this route a 401 means the code, not a token.
      throw TernException(e.message, status: 401);
    } finally {
      client.close();
    }
  }

  Future<Health> health() async =>
      Health.fromJson(await _json('GET', '/v1/health'));

  Future<Info> info() async => Info.fromJson(await _json('GET', '/v1/info'));

  /// Version, uptime and counts, cheap enough to poll.
  Future<DaemonStatus> status() async =>
      DaemonStatus.fromJson(await _json('GET', '/v1/status'));

  /// Stop tern (exit code 0, so a service manager doesn't restart it).
  /// Runs in progress stay paused and resume on the next start. With
  /// [dryRun], only list them, for the confirmation. Owner-only remotely.
  Future<ShutdownReply> shutdown({bool dryRun = false}) async =>
      ShutdownReply.fromJson(
        await _json('POST', dryRun ? '/v1/shutdown?dry_run=1' : '/v1/shutdown'),
      );

  /// Every tool the agent has, with its argument schema.
  Future<List<ToolSpec>> tools() async => [
    for (final t in await _json('GET', '/v1/tools') as List)
      ToolSpec.fromJson(t as Json),
  ];

  /// Resolve `model` (the provider may be left off when only one lists
  /// it). Throws [TernException] with the daemon's reason if it's unusable.
  Future<Resolved> checkModel(String model) async => Resolved.fromJson(
    await _json('POST', '/v1/models/check', {'model': model}),
    model,
  );

  /// Configured providers, with masked keys.
  Future<List<ProviderStatus>> providers() async => [
    for (final p in await _json('GET', '/v1/providers') as List)
      ProviderStatus.fromJson(p as Json),
  ];

  /// Model lists, from the daemon's cache unless `refresh`.
  Future<List<ProviderModels>> models({
    String? provider,
    bool refresh = false,
  }) async {
    final q = [
      if (provider != null) 'provider=${Uri.encodeQueryComponent(provider)}',
      if (refresh) 'refresh=true',
    ].join('&');
    return [
      for (final m
          in await _json('GET', '/v1/models${q.isEmpty ? '' : '?$q'}') as List)
        ProviderModels.fromJson(m as Json),
    ];
  }

  /// Check a new provider, or a change to `existing`, with the provider
  /// itself. Saves nothing.
  Future<Verified> verifyProvider(
    ProviderDraft draft, {
    String? existing,
  }) async => Verified.fromJson(
    await _json(
      'POST',
      existing == null
          ? '/v1/providers/verify'
          : '/v1/providers/$existing/verify',
      draft.toJson(),
    ),
  );

  /// Verify, store the key, write the config and reload providers. Throws
  /// [TernException] with the reason when the key is rejected.
  Future<ProviderStatus> saveProvider(
    ProviderDraft draft, {
    String? existing,
    String? defaultModel,
  }) async {
    final body = {...draft.toJson(), 'default_model': ?defaultModel};
    final j =
        await _json(
              existing == null ? 'POST' : 'PATCH',
              existing == null ? '/v1/providers' : '/v1/providers/$existing',
              body,
            )
            as Json;
    return ProviderStatus.fromJson(j['provider'] as Json);
  }

  /// Remove a provider and, with `removeSecret`, its key (unless
  /// something else uses it).
  Future<void> removeProvider(String name, {bool removeSecret = true}) =>
      _json('DELETE', '/v1/providers/$name?remove_secret=$removeSecret');

  /// Make `model` the default for new conversations.
  Future<Resolved> setDefaultModel(String model) async {
    final j =
        await _json('PUT', '/v1/config/default_model', {'model': model})
            as Json;
    return Resolved.fromJson(j, model);
  }

  /// Configured MCP servers with their state. Check `features` in
  /// [health] for `mcp` first.
  Future<List<McpStatus>> mcpServers() async => [
    for (final s in await _json('GET', '/v1/mcp') as List)
      McpStatus.fromJson(s as Json),
  ];

  /// One server, including its stderr tail.
  Future<McpStatus> mcpServer(String name) async =>
      McpStatus.fromJson(await _json('GET', '/v1/mcp/${_seg(name)}'));

  /// Test-connect a draft with `secrets` typed but not yet saved. Saves
  /// nothing; a failed connection is a result, not an exception. A paired
  /// device gets 403 for a stdio config unless the host allows it.
  Future<McpVerifyResult> verifyMcp(
    String name,
    McpServerConfig config, {
    Map<String, String> secrets = const {},
  }) async => McpVerifyResult.fromJson(
    await _json('POST', '/v1/mcp/verify', {
      'name': name,
      'config': config.toJson(),
      if (secrets.isNotEmpty) 'secrets': secrets,
    }),
  );

  /// Read a pasted `mcpServers` snippet into drafts. Saves nothing; 400
  /// when nothing in it could be imported.
  Future<List<ImportedServer>> importMcp(String text) async => [
    for (final s
        in await _json('POST', '/v1/mcp/import', {'text': text}) as List)
      ImportedServer.fromJson(s as Json),
  ];

  /// Store `secrets`, write the server and start it. 409 if the name is
  /// taken.
  Future<McpStatus> addMcp(
    String name,
    McpServerConfig config, {
    Map<String, String> secrets = const {},
  }) async => McpStatus.fromJson(
    await _json('POST', '/v1/mcp', {
      'name': name,
      'config': config.toJson(),
      if (secrets.isNotEmpty) 'secrets': secrets,
    }),
  );

  /// Replace `old`'s config; a different `name` renames it.
  Future<McpStatus> updateMcp(
    String old, {
    String? name,
    required McpServerConfig config,
    Map<String, String> secrets = const {},
  }) async => McpStatus.fromJson(
    await _json('PUT', '/v1/mcp/${_seg(old)}', {
      'name': ?name,
      'config': config.toJson(),
      if (secrets.isNotEmpty) 'secrets': secrets,
    }),
  );

  /// The quick toggles; fields left null don't change.
  Future<McpStatus> patchMcp(
    String name, {
    bool? enabled,
    List<String>? disabledTools,
    bool? requiresApproval,
    bool? idempotent,
  }) async => McpStatus.fromJson(
    await _json('PATCH', '/v1/mcp/${_seg(name)}', {
      'enabled': ?enabled,
      'disabled_tools': ?disabledTools,
      'requires_approval': ?requiresApproval,
      'idempotent': ?idempotent,
    }),
  );

  /// Remove a server and, with `removeSecrets`, the secrets nothing else
  /// uses.
  Future<void> removeMcp(String name, {bool removeSecrets = false}) =>
      _json('DELETE', '/v1/mcp/${_seg(name)}?remove_secrets=$removeSecrets');

  /// Reconnect now, clearing any backoff.
  Future<McpStatus> restartMcp(String name) async =>
      McpStatus.fromJson(await _json('POST', '/v1/mcp/${_seg(name)}/restart'));

  /// Recent conversations; with `channel`, that channel's threads (for
  /// Home: its History).
  Future<List<SessionItem>> sessions({int limit = 50, String? channel}) async =>
      [
        for (final s
            in await _json(
                  'GET',
                  '/v1/sessions?limit=$limit'
                      '${channel == null ? '' : '&channel=${_seg(channel)}'}',
                )
                as List)
          SessionItem.fromJson(s as Json),
      ];

  // ---- agent profiles (docs/agent-profiles-design.md §7.1) ----

  /// Agents, the default first; with `archived`, archived ones too.
  Future<List<Agent>> agents({bool archived = false}) async => [
    for (final a
        in await _json('GET', '/v1/agents${archived ? '?archived=1' : ''}')
            as List)
      Agent.fromJson(a as Json),
  ];

  /// Every agent including archived and deleted ones (marked), so history
  /// can still name what a deleted agent wrote.
  Future<List<Agent>> agentsIncludingDeleted() async => [
    for (final a in await _json('GET', '/v1/agents?deleted=1') as List)
      Agent.fromJson(a as Json),
  ];

  /// What deleting an agent touches, and what stops it.
  Future<AgentImpact> agentImpact(String id) async => AgentImpact.fromJson(
    await _json('GET', '/v1/agents/${_seg(id)}/impact') as Json,
  );

  /// Delete an agent for good (its history stays). `reassignTo` hands its
  /// routines and led channels to another agent; `deleteRoutines` deletes
  /// them. Throws [TernException] (409) when it can't be done.
  Future<void> deleteAgent(
    String id, {
    String? reassignTo,
    bool deleteRoutines = false,
  }) => _json(
    'DELETE',
    '/v1/agents/${_seg(id)}?confirm=true'
    '${reassignTo == null ? '' : '&reassign_to=${Uri.encodeQueryComponent(reassignTo)}'}'
    '${deleteRoutines ? '&delete_routines=true' : ''}',
  );

  Future<List<AgentTemplate>> agentTemplates() async => [
    for (final t in await _json('GET', '/v1/agents/templates') as List)
      AgentTemplate.fromJson(t as Json),
  ];

  /// Create an agent and its DM. `model` is one model or a chain. Throws
  /// [TernException] (409) if the name is taken.
  Future<Agent> createAgent({
    required String name,
    String? slug,
    AvatarSpec? avatar,
    String? persona,
    List<String>? model,
    List<String>? subagentModel,
    ToolScope? tools,
    String? memory,
    String? egress,
    String? autonomy,
    String? template,
  }) async => Agent.fromJson(
    await _json('POST', '/v1/agents', {
      'name': name,
      'slug': ?slug,
      'avatar': ?avatar?.toJson(),
      'persona': ?persona,
      if (model != null && model.isNotEmpty)
        'model': model.length == 1 ? model.first : model,
      if (subagentModel != null && subagentModel.isNotEmpty)
        'subagent_model': subagentModel.length == 1
            ? subagentModel.first
            : subagentModel,
      'tools': ?tools?.toJson(),
      'memory': ?memory,
      'egress': ?egress,
      'autonomy': ?autonomy,
      'template': ?template,
    }),
  );

  /// Change an agent: `fields` as the API takes them (`null` clears one,
  /// `archived: true` archives it).
  Future<Agent> updateAgent(String id, Json fields) async =>
      Agent.fromJson(await _json('PATCH', '/v1/agents/${_seg(id)}', fields));

  /// Set an agent's autonomy ceiling (`readonly`, `supervised`,
  /// `autonomous`, `yolo`), or remove it with null.
  Future<Agent> setAgentAutonomy(String id, String? level) =>
      updateAgent(id, {'autonomy': level});

  /// Give an agent a Telegram bot of its own, by the NAME of the secret that
  /// holds its token (never the token), or remove it with null. Throws
  /// [TernException] (400) when the name is bad or another bot uses it.
  /// Applies when the daemon restarts.
  Future<Agent> setAgentTelegram(String id, String? secret) => updateAgent(id, {
    'telegram': secret == null ? null : {'bot_token_secret': secret},
  });

  /// What `id` did lately: delegated batches and routine runs.
  Future<List<AgentWork>> agentWork(String id, {int limit = 10}) async => [
    for (final w
        in await _json('GET', '/v1/agents/${_seg(id)}/work?limit=$limit')
            as List)
      AgentWork.fromJson(w as Json),
  ];

  Future<Persona> persona(String id) async =>
      Persona.fromJson(await _json('GET', '/v1/agents/${_seg(id)}/persona'));

  Future<void> setPersona(String id, String persona) =>
      _json('PUT', '/v1/agents/${_seg(id)}/persona', {'persona': persona});

  /// Memories: `agent` is `shared`, an agent (its own), or null for all.
  Future<List<Memory>> memories({String? agent, String? query}) async {
    final q = [
      if (agent != null) 'agent=${Uri.encodeQueryComponent(agent)}',
      if (query != null && query.isNotEmpty)
        'q=${Uri.encodeQueryComponent(query)}',
    ].join('&');
    return [
      for (final m
          in await _json('GET', '/v1/memories${q.isEmpty ? '' : '?$q'}')
              as List)
        Memory.fromJson(m as Json),
    ];
  }

  Future<List<Channel>> channels({bool archived = false}) async => [
    for (final c
        in await _json('GET', '/v1/channels${archived ? '?archived=1' : ''}')
            as List)
      Channel.fromJson(c as Json),
  ];

  /// A topic channel. Throws [TernException] (409) if the slug is taken.
  Future<Channel> createChannel({
    required String slug,
    String? title,
    String? instructions,
  }) async => Channel.fromJson(
    await _json('POST', '/v1/channels', {
      'slug': slug,
      'title': ?title,
      'instructions': ?instructions,
    }),
  );

  /// What deleting a topic channel touches, and what stops it.
  Future<ChannelImpact> channelImpact(String id) async =>
      ChannelImpact.fromJson(
        await _json('GET', '/v1/channels/${_seg(id)}/impact') as Json,
      );

  /// Delete a topic channel for good: its posts go, its conversations stay
  /// in History. Throws [TernException] (409) when it can't be done.
  Future<void> deleteChannel(String id) =>
      _json('DELETE', '/v1/channels/${_seg(id)}?confirm=true');

  /// `clearInstructions` removes them; `instructions` sets them; `slug`
  /// renames a topic channel's #name.
  Future<Channel> updateChannel(
    String id, {
    String? slug,
    String? title,
    String? instructions,
    bool clearInstructions = false,
    bool? archived,
    String? lead,
    bool? agentWakes,
    int? maxHops,
    int? wakesPerHour,
    bool? letAgentsDecide,
  }) async => Channel.fromJson(
    await _json('PATCH', '/v1/channels/${_seg(id)}', {
      'slug': ?slug,
      'title': ?title,
      if (clearInstructions)
        'instructions': null
      else
        'instructions': ?instructions,
      'archived': ?archived,
      'lead': ?lead,
      'agent_wakes': ?agentWakes,
      'max_hops': ?maxHops,
      'wakes_per_hour': ?wakesPerHour,
      'let_agents_decide': ?letAgentsDecide,
    }),
  );

  /// Add an agent (id or slug) to a topic channel.
  Future<ChannelAgents> addChannelAgent(String channel, String agent) async =>
      ChannelAgents.fromJson(
        await _json('POST', '/v1/channels/${_seg(channel)}/agents', {
          'agent': agent,
        }),
      );

  /// Take an agent out of a topic channel (not its lead or `tern`).
  Future<ChannelAgents> removeChannelAgent(
    String channel,
    String agent,
  ) async => ChannelAgents.fromJson(
    await _json(
      'DELETE',
      '/v1/channels/${_seg(channel)}/agents/${_seg(agent)}',
    ),
  );

  /// A page of a channel's posts, newest first.
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async => [
    for (final p
        in await _json(
              'GET',
              '/v1/channels/${_seg(channel)}/posts?limit=$limit'
                  '${before == null ? '' : '&before=$before'}',
            )
            as List)
      Post.fromJson(p as Json),
  ];

  /// Send into a channel: Home's stream, or a topic channel's main thread
  /// (a side thread with `replyTo`). A run already working there gets the
  /// text as a message instead of being stopped; `newTopic` starts a new
  /// segment of Home.
  Future<Sent> sendToChannel(
    String channel,
    String text, {
    int? replyTo,
    bool newTopic = false,
  }) async => Sent.fromJson(
    await _json('POST', '/v1/channels/${_seg(channel)}/posts', {
      'text': text,
      'reply_to': ?replyTo,
      if (newTopic) 'new_topic': true,
    }),
  );

  /// Move conversations into a topic channel (`channel`, an id or slug)
  /// or a new one (`newChannelTitle`). Owner scope. Throws with 409 when
  /// something is still running in one of them.
  Future<MoveResult> moveToChannel(
    List<String> sessions, {
    String? channel,
    String? newChannelTitle,
  }) async => MoveResult.fromJson(
    await _json('POST', '/v1/channels/move', {
      'sessions': sessions,
      'channel': ?channel,
      if (newChannelTitle != null) 'new_channel': {'title': newChannelTitle},
    }),
  );

  /// Accept a channel suggestion: create the channel and, unless told
  /// not to, move the conversation and the suggested routines there.
  /// Returns the new channel's id.
  Future<String> acceptSuggestion(
    int id, {
    bool moveConversation = true,
    List<String>? routines,
  }) async {
    final j = await _json('POST', '/v1/suggestions/$id/accept', {
      'move_conversation': moveConversation,
      'routines': ?routines,
    });
    return ((j as Json)['channel'] as Json)['id'] as String;
  }

  Future<void> declineSuggestion(int id) =>
      _json('POST', '/v1/suggestions/$id/decline');

  /// Mark a `needs_you` post or its pointer in Home done (both close).
  Future<void> resolvePost(int id) => _json('POST', '/v1/posts/$id/resolve');

  /// A page of a main channel's stream, newest first.
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async => StreamPage.fromJson(
    await _json(
      'GET',
      '/v1/channels/${_seg(channel)}/stream?limit=$limit'
          '${before == null ? '' : '&before=${Uri.encodeQueryComponent(before)}'}',
    ),
  );

  Future<SessionDetail> session(String id) async =>
      SessionDetail.fromJson(await _json('GET', '/v1/sessions/$id'));

  Future<void> setModel(String session, String model) =>
      _json('PUT', '/v1/sessions/$session/model', {'model': model});

  Future<Usage> usage(String session) async =>
      Usage.fromJson(await _json('GET', '/v1/sessions/$session/usage'));

  /// Send a message. Without `session` the daemon starts a new
  /// conversation. A run already in progress in that session is cancelled
  /// first. (Conversations in a channel go through [sendToChannel].)
  Future<Started> send(String text, {String? session, String? model}) async =>
      Started.fromJson(
        await _json('POST', '/v1/runs', {
          'text': text,
          'session_id': ?session,
          'model': ?model,
        }),
      );

  Future<bool> cancel(String run) async =>
      (await _json('POST', '/v1/runs/$run/cancel'))['cancelled'] as bool;

  Future<void> resume(String run) => _json('POST', '/v1/runs/$run/resume');

  /// Every sub-agent started from this conversation's runs.
  Future<List<ChildRun>> subagents(String session) async => [
    for (final r
        in await _json('GET', '/v1/sessions/$session/subagents') as List)
      ChildRun.fromJson(r as Json),
  ];

  /// Tell a running sub-agent something, as the user.
  Future<void> messageRun(String run, String text) =>
      _json('POST', '/v1/runs/$run/message', {'text': text});

  Future<List<ActiveRun>> activeRuns() async => [
    for (final r in await _json('GET', '/v1/runs/active') as List)
      ActiveRun.fromJson(r as Json),
  ];

  Future<List<Approval>> pendingApprovals() async => [
    for (final a in await _json('GET', '/v1/approvals') as List)
      Approval.fromJson(a as Json),
  ];

  /// Secret requests waiting for a value (resync). Check `features` in
  /// [health] for `secret_requests` first.
  Future<List<SecretRequest>> secretRequests() async => [
    for (final r in await _json('GET', '/v1/secret-requests') as List)
      SecretRequest.fromJson(r as Json),
  ];

  /// Send the values the user entered. Names left out are skipped; an empty
  /// map skips everything. Throws [TernException] with status 404 if the
  /// request is no longer pending, 403 without owner scope.
  Future<void> answerSecretRequest(String id, Map<String, String> values) =>
      _json('POST', '/v1/secret-requests/$id', {'values': values});

  /// Open sign-in and connect cards (resync). Check `features` in
  /// [health] for `connections` first.
  Future<List<AuthPrompt>> authPrompts() async => [
    for (final p in await _json('GET', '/v1/auth-prompts') as List)
      AuthPrompt.fromJson(p as Json),
  ];

  /// Answer a card with one of its option ids, `device` or `cancel`. 400 if
  /// the card doesn't offer that, 404 if it is gone.
  Future<void> answerAuthPrompt(String id, String answer) =>
      _json('POST', '/v1/auth-prompts/${_seg(id)}', {'answer': answer});

  /// The accounts, and everything one can be added to. Owner-only.
  Future<ConnectionList> connections() async =>
      ConnectionList.fromJson(await _json('GET', '/v1/connections'));

  /// Add an account. With `method: oauth` this waits (up to ~5 minutes) for
  /// the user to sign in; the sign-in card arrives on the event stream
  /// meanwhile. `values` (keyed by the target's `key_names`) go only in the
  /// body. 409 means cancelled or declined, 408 timed out.
  Future<ConnectResult> addConnection({
    required String kind,
    required String target,
    String? connection,
    String? label,
    String? method,
    bool device = false,
    Map<String, String> values = const {},
  }) async => ConnectResult.fromJson(
    await _json('POST', '/v1/connections', {
      'kind': kind,
      'target': target,
      'connection': ?connection,
      'label': ?label,
      'method': ?method,
      if (device) 'device': true,
      if (values.isNotEmpty) 'values': values,
    }),
  );

  static String _acct(Account a) =>
      '/v1/connections/${_seg(a.kind)}/${_seg(a.target)}/${_seg(a.slug)}';

  /// Sign `a` in again (or replace its keys).
  Future<ConnectResult> reconnectConnection(
    Account a, {
    bool device = false,
    Map<String, String> values = const {},
  }) async => ConnectResult.fromJson(
    await _json('POST', '${_acct(a)}/reconnect', {
      if (device) 'device': true,
      if (values.isNotEmpty) 'values': values,
    }),
  );

  Future<Account> makeDefaultConnection(Account a) async =>
      Account.fromJson(await _json('POST', '${_acct(a)}/default'));

  Future<Account> renameConnection(Account a, String label) async =>
      Account.fromJson(await _json('PATCH', _acct(a), {'label': label}));

  /// Revoke at the service where possible, then delete its tokens, keys,
  /// cache and grants.
  Future<void> removeConnection(Account a) => _json('DELETE', _acct(a));

  Future<List<Schedule>> schedules() async => [
    for (final s in await _json('GET', '/v1/schedules') as List)
      Schedule.fromJson(s as Json),
  ];

  /// `deliverTo`: null keeps results in the routine's session; else
  /// `telegram:<chat_id>`.
  Future<Schedule> createSchedule({
    required String cron,
    required String prompt,
    String? deliverTo,
    ScheduleLimits? limits,
    String? channel,
    String? agent,
    RoutineOptions? options,
  }) async => Schedule.fromJson(
    await _json('POST', '/v1/schedules', {
      if (!(options?.replacesCron ?? false) && cron.isNotEmpty) 'cron': cron,
      'prompt': prompt,
      'deliver_to': ?deliverTo,
      'channel': ?channel,
      'agent': ?agent,
      ...?limits?.toJson(),
      ...?options?.toJson(),
    }),
  );

  /// Pass `deliverTo: 'none'` to stop delivering. `limits` replaces all of
  /// them (nulls put the defaults back); `resume` lifts a budget pause.
  Future<Schedule> updateSchedule(
    String id, {
    String? cron,
    String? prompt,
    bool? enabled,
    String? deliverTo,
    ScheduleLimits? limits,
    bool resume = false,
    String? channel,
    String? agent,
    RoutineOptions? options,
  }) async => Schedule.fromJson(
    await _json('PATCH', '/v1/schedules/$id', {
      if (!(options?.replacesCron ?? false) && (cron?.isNotEmpty ?? false))
        'cron': cron,
      'prompt': ?prompt,
      'enabled': ?enabled,
      'deliver_to': ?deliverTo,
      'channel': ?channel,
      'agent': ?agent,
      ...?limits?.toJson(),
      ...?options?.toJson(),
      if (resume) 'resume': true,
    }),
  );

  Future<void> deleteSchedule(String id) =>
      _json('DELETE', '/v1/schedules/$id');

  /// Owner only: every webhook routine's hook (never secrets).
  Future<List<Webhook>> webhooks() async => [
    for (final w in await _json('GET', '/v1/webhooks') as List)
      Webhook.fromJson(w as Json),
  ];

  /// Owner only: a routine run by webhook deliveries. The answer carries
  /// the secret, which the daemon never shows again. `auth`: `hmac` or
  /// `bearer`.
  Future<CreatedWebhook> createWebhook({
    required String prompt,
    String auth = 'hmac',
    String? deliverTo,
    ScheduleLimits? limits,
    String? channel,
    String? agent,
    RoutineOptions? options,
  }) async => CreatedWebhook.fromJson(
    await _json('POST', '/v1/webhooks', {
      'prompt': prompt,
      'auth': auth,
      'deliver_to': ?deliverTo,
      'channel': ?channel,
      'agent': ?agent,
      ...?limits?.toJson(),
      ...?options?.toJson(),
    }),
  );

  /// A new secret at once (the old one stops working); also turns a
  /// revoked hook back on.
  Future<Webhook> rotateWebhook(String id, {String? auth}) async =>
      Webhook.fromJson(
        await _json('POST', '/v1/webhooks/$id/rotate', {'auth': ?auth}),
      );

  /// The URL answers 404 until [rotateWebhook].
  Future<void> revokeWebhook(String id) => _json('DELETE', '/v1/webhooks/$id');

  /// Owner only: installed bundles (tooling spec §6.5).
  Future<List<InstalledBundle>> bundles() async => [
    for (final b
        in (await _json('GET', '/v1/bundles') as Json)['bundles'] as List)
      InstalledBundle.fromJson(b as Json),
  ];

  /// Everything a bundle (an https URL, `…#sha256=` to pin it, or a path on
  /// the core's machine) would do. Installs nothing.
  Future<BundlePreview> previewBundle(String source, {String? sha256}) async =>
      BundlePreview.fromJson(
        await _json('POST', '/v1/bundles/preview', {
          'source': source,
          'sha256': ?sha256,
        }),
      );

  /// Install what [previewBundle] showed: `sha256` is the plan's, so a
  /// bundle that changed since is refused.
  Future<BundleOutcome> installBundle(
    String source, {
    required String sha256,
    String? agent,
    bool acceptNewKey = false,
  }) async => BundleOutcome.fromJson(
    await _json('POST', '/v1/bundles/install', {
      'source': source,
      'sha256': sha256,
      'agent': ?agent,
      if (acceptNewKey) 'accept_new_key': true,
    }),
  );

  /// Remove a bundle and the connectors, skills and routines it installed.
  Future<BundleOutcome> removeBundle(String name) async =>
      BundleOutcome.fromJson(
        await _json('DELETE', '/v1/bundles/${Uri.encodeComponent(name)}'),
      );

  /// The global initiative dial: `vacation`, `quieter`, `normal`, `louder`.
  Future<String> initiative() async =>
      (await _json('GET', '/v1/initiative'))['dial'] as String;

  Future<String> setInitiative(String dial) async =>
      (await _json('PUT', '/v1/initiative', {'dial': dial}))['dial'] as String;

  /// Run a routine now; its next scheduled run doesn't move.
  Future<Started> runSchedule(String id) async {
    final j = await _json('POST', '/v1/schedules/$id/run') as Json;
    return Started.fromJson({...j, 'model': ''});
  }

  /// Validate `cron` and get its next three runs. Throws [TernException]
  /// with the reason if it's invalid.
  Future<CronPreview> previewCron(String cron) async => CronPreview.fromJson(
    await _json('POST', '/v1/schedules/preview', {'cron': cron}),
  );

  /// A conversation's runs; with [subagents], the runs they started too.
  Future<List<RunItem>> runs(String session, {bool subagents = false}) async =>
      [
        for (final r
            in await _json(
                  'GET',
                  '/v1/sessions/$session/runs'
                      '${subagents ? '?include=subagents' : ''}',
                )
                as List)
          RunItem.fromJson(r as Json),
      ];

  Future<List<StepItem>> steps(String run) async => [
    for (final s in await _json('GET', '/v1/runs/$run/steps') as List)
      StepItem.fromJson(s as Json),
  ];

  /// Every secret in `secrets.toml` or referred to by config or a
  /// connector. Values are never returned.
  Future<List<SecretStatus>> secrets() async => [
    for (final s in await _json('GET', '/v1/secrets') as List)
      SecretStatus.fromJson(s as Json),
  ];

  /// Store `value` in `secrets.toml`. Takes effect without a restart.
  Future<SecretStatus> setSecret(String name, String value) async =>
      SecretStatus.fromJson(
        await _json('PUT', '/v1/secrets/$name', {'value': value}),
      );

  /// `config.toml` as settings, by section. Owner scope only.
  Future<ConfigView> configSettings() async =>
      ConfigView.fromJson(await _json('GET', '/v1/config/settings'));

  /// Set one setting to a JSON `value` (`null` unsets an optional one).
  /// The daemon checks it and keeps the file's comments.
  Future<ConfigSetting> setSetting(String key, Object? value) async =>
      ConfigSetting.fromJson(
        await _json('PUT', '/v1/config/settings/${_seg(key)}', {
          'value': value,
        }),
      );

  /// Back to the default: removed from `config.toml`.
  Future<ConfigSetting> resetSetting(String key) async =>
      ConfigSetting.fromJson(
        await _json('DELETE', '/v1/config/settings/${_seg(key)}'),
      );

  /// Remove `name` from `secrets.toml`. Returns false if it wasn't there.
  Future<bool> removeSecret(String name) async {
    try {
      await _json('DELETE', '/v1/secrets/$name');
      return true;
    } on TernException catch (e) {
      if (e.status == 404) return false;
      rethrow;
    }
  }

  Future<void> answer(String approval, Decision decision) =>
      _json('POST', '/v1/approvals/$approval', {'decision': decision.wire});

  /// Outbox items waiting for the user (`all`: decided ones too). Check
  /// `features` in [health] for `outbox` first.
  Future<List<OutboxItem>> outbox({bool all = false}) async => [
    for (final o
        in await _json('GET', all ? '/v1/outbox?state=all' : '/v1/outbox')
            as List)
      OutboxItem.fromJson(o as Json),
  ];

  /// Activity ledger entries, newest first (owner only; check `features`
  /// in [health] for `ledger`). [kind] is a kind (`grant.add`) or a family
  /// (`grant`); page back with [before].
  Future<List<LedgerEntry>> ledger({
    String? kind,
    String? channel,
    String? routine,
    String? connector,
    String? run,
    int? before,
    int? limit,
  }) async {
    final q = {
      'kind': ?kind,
      'channel': ?channel,
      'routine': ?routine,
      'connector': ?connector,
      'run': ?run,
      'before': ?before?.toString(),
      'limit': ?limit?.toString(),
    };
    final path = q.isEmpty
        ? '/v1/ledger'
        : '/v1/ledger?${Uri(queryParameters: q).query}';
    return [
      for (final e in await _json('GET', path) as List)
        LedgerEntry.fromJson(e as Json),
    ];
  }

  /// Everything running, queued, scheduled, waiting on you or failed
  /// recently (check `features` in [health] for `tasks`).
  Future<Tasks> tasks() async =>
      Tasks.fromJson(await _json('GET', '/v1/tasks') as Json);

  /// `tern security audit` on the core (owner only; check `features` in
  /// [health] for `security_audit`).
  Future<SecurityAudit> securityAudit() async =>
      SecurityAudit.fromJson(await _json('GET', '/v1/security/audit') as Json);

  /// Recompute the ledger's hash chain.
  Future<LedgerCheck> verifyLedger() async =>
      LedgerCheck.fromJson(await _json('GET', '/v1/ledger/verify') as Json);

  /// Run item [id] now, exactly once; [input] replaces its arguments.
  /// Throws [TernException] with 409 if it was already decided or expired.
  Future<OutboxItem> approveOutbox(int id, {Object? input}) async =>
      OutboxItem.fromJson(
        await _json('POST', '/v1/outbox/$id/approve', {'input': ?input})
            as Json,
      );

  /// Don't run item [id]; [note] goes to the run that queued it.
  Future<OutboxItem> rejectOutbox(int id, {String? note}) async =>
      OutboxItem.fromJson(
        await _json('POST', '/v1/outbox/$id/reject', {'note': ?note}) as Json,
      );

  /// Put a file in the core's workspace (remote cores can't take a path).
  /// Returns its workspace-relative path, to mention in a message. Check
  /// `features` in [health] for `uploads` first.
  Future<String> upload(String name, List<int> bytes) async {
    final boundary = 'tern-${DateTime.now().microsecondsSinceEpoch}';
    final safe = name.replaceAll(RegExp(r'["\r\n]'), '_');
    final body = <int>[
      ...utf8.encode(
        '--$boundary\r\n'
        'Content-Disposition: form-data; name="file"; filename="$safe"\r\n'
        'Content-Type: application/octet-stream\r\n\r\n',
      ),
      ...bytes,
      ...utf8.encode('\r\n--$boundary--\r\n'),
    ];
    final j = await _request(
      'POST',
      '/v1/uploads',
      body,
      contentType: ContentType(
        'multipart',
        'form-data',
        parameters: {'boundary': boundary},
      ),
    );
    return (j as Json)['path'] as String;
  }

  /// Owner only: paired devices.
  Future<List<Device>> devices() async => [
    for (final d in await _json('GET', '/v1/devices') as List)
      Device.fromJson(d as Json),
  ];

  /// Owner only: a pairing link for a new device (the app's Add device).
  Future<PairingOffer> offerPairing({
    String scope = 'chat',
    String? name,
  }) async => PairingOffer.fromJson(
    await _json('POST', '/v1/devices/pair', {'scope': scope, 'name': ?name}),
  );

  /// Owner only: sign a device out at once.
  Future<void> revokeDevice(String id) => _json('DELETE', '/v1/devices/$id');

  /// The live event stream. It ends (with an error) when the connection
  /// drops; callers reconnect and resync, except after a
  /// [SignedOutException], which no retry fixes. Cancelling closes the
  /// connection.
  Stream<TernEvent> events() {
    // Its own HttpClient: cancelling an open response doesn't return until
    // the server ends it, so the connection is force-closed instead.
    final http = endpoint.httpClient();
    final out = StreamController<TernEvent>();
    out.onListen = () async {
      final HttpClientResponse res;
      try {
        res = await _send(
          'GET',
          '/v1/events',
          null,
          client: http,
          accept: 'text/event-stream',
        );
      } on TernException catch (e) {
        out.addError(e);
        await out.close();
        return;
      }
      if (res.statusCode >= 400) {
        final text = await res.transform(utf8.decoder).join();
        String? msg;
        try {
          final j = jsonDecode(text);
          if (j is Json) msg = j['error'] as String?;
        } on FormatException {
          // Not JSON; use the status.
        }
        out.addError(_error(res.statusCode, msg));
        await out.close();
        return;
      }
      res
          .transform(const SseDecoder())
          .listen(
            (data) {
              final Object? decoded;
              try {
                decoded = jsonDecode(data);
              } on FormatException {
                return;
              }
              if (decoded is Json) out.add(TernEvent.fromJson(decoded));
            },
            onError: (Object e) =>
                out.addError(TernException('event stream lost: $e')),
            onDone: out.close,
            cancelOnError: true,
          );
    };
    out.onCancel = () => http.close(force: true);
    return out.stream;
  }

  static String _seg(String s) => Uri.encodeComponent(s);

  Future<dynamic> _json(String method, String path, [Object? body]) =>
      _request(method, path, body);

  Future<dynamic> _request(
    String method,
    String path,
    Object? body, {
    ContentType? contentType,
    bool auth = true,
  }) async {
    final res = await _send(
      method,
      path,
      body,
      contentType: contentType,
      auth: auth,
    );
    final text = await res.transform(utf8.decoder).join();
    Object? decoded;
    try {
      decoded = text.isEmpty ? null : jsonDecode(text);
    } on FormatException {
      decoded = text;
    }
    if (res.statusCode >= 400) {
      final msg = decoded is Json ? decoded['error'] as String? : null;
      throw _error(res.statusCode, msg);
    }
    return decoded;
  }

  TernException _error(int status, String? msg) {
    if (status == HttpStatus.unauthorized && endpoint is RemoteEndpoint) {
      return SignedOutException(
        'Signed out of ${endpoint.describe()}: ${msg ?? 'the device token was revoked'}',
        core: endpoint.describe(),
      );
    }
    return TernException(msg ?? 'HTTP $status', status: status);
  }

  Future<HttpClientResponse> _send(
    String method,
    String path,
    Object? body, {
    HttpClient? client,
    String accept = 'application/json',
    ContentType? contentType,
    bool auth = true,
  }) async {
    try {
      final req = await (client ?? _http).openUrl(
        method,
        endpoint.base.resolve(path),
      );
      req.headers.set(HttpHeaders.acceptHeader, accept);
      if (auth) endpoint.headers.forEach(req.headers.set);
      if (body is List<int> && contentType != null) {
        req.headers.contentType = contentType;
        req.contentLength = body.length;
        req.add(body);
      } else if (body != null) {
        req.headers.contentType = ContentType.json;
        req.write(jsonEncode(body));
      }
      return await req.close();
    } on SocketException catch (e) {
      throw TernException(
        "can't reach tern at ${endpoint.describe()}: "
        '${e.osError?.message ?? e.message}',
      );
    } on TlsException catch (e) {
      // With a pin, this is also what a different certificate looks like.
      throw TernException(
        "can't establish a secure connection to ${endpoint.describe()}: "
        '${e.osError?.message ?? e.message} '
        "(the core's certificate isn't the one this device paired with?)",
      );
    } on HttpException catch (e) {
      throw TernException(e.message);
    }
  }
}
