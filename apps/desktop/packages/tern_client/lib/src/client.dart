import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'agents.dart';
import 'bundles.dart';
import 'chain.dart';
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
import 'workspaces.dart';

/// An error response from the daemon (`{"error": "..."}`) or a failure to
/// reach it.
class TernException implements Exception {
  const TernException(this.message, {this.status, this.body});

  final String message;
  final int? status;

  /// The error response's JSON, when it had one (extra fields such as a
  /// folder `verdict` or an `impact`).
  final Map<String, dynamic>? body;

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
    final problem = remoteUrlProblem(link.url);
    if (problem != null) throw TernException(problem);
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

  /// Whether a tern server answers at [url] (its `/healthz`, which needs no
  /// token), checking its certificate against [pin] when given. Sends no
  /// credentials, so it is safe to try before anything is saved. Throws
  /// [TernException]: unreachable (status null), a bad certificate, or an
  /// answer that isn't tern's.
  static Future<void> probe(
    Uri url, {
    String? pin,
    Duration timeout = const Duration(seconds: 8),
  }) async {
    final problem = remoteUrlProblem(url);
    if (problem != null) throw TernException(problem);
    final client = TernClient(RemoteEndpoint(url, token: '', pin: pin));
    try {
      await client
          ._request('GET', '/healthz', null, auth: false)
          .timeout(
            timeout,
            onTimeout: () => throw TernException(
              "can't reach tern at ${client.endpoint.describe()}: timed out",
            ),
          );
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

  /// The friendly presets ("Connect a provider"), in display order.
  Future<List<ProviderPreset>> providerPresets() async => [
    for (final p in await _json('GET', '/v1/providers/presets') as List)
      ProviderPreset.fromJson(p as Json),
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

  /// Set `default_model` to a chain: the primary first, then the fallbacks
  /// tried in order (T6a). The daemon checks every entry and keeps the
  /// file's comments; it applies to the next message. Owner only, as every
  /// config write is.
  Future<ConfigSetting> setDefaultModelChain(List<String> chain) {
    if (chain.isEmpty) {
      throw ArgumentError('a default model needs at least one model');
    }
    return setSetting('default_model', chainToWire(chain));
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

  /// Opt an agent in to (or out of) code auto mode. Turning it on needs a
  /// ceiling of `code` or above after the change: pass [raiseCeiling] when the
  /// agent's is lower (the confirm dialog said so), which sends
  /// `autonomy: "code"` in the same request. Throws [TernException] (409,
  /// `ceiling_below_code`) when the ceiling would stay below `code`, or the
  /// agent is read-only.
  Future<Agent> setCodeAuto(String id, bool on, {bool raiseCeiling = false}) =>
      updateAgent(id, {
        'code_auto': on,
        if (on && raiseCeiling) 'autonomy': 'code',
      });

  /// What a code auto mode run changed since its pre-run snapshot, and why
  /// there is none to compare with (when there isn't one).
  Future<RunChanges> runChanges(String runId) async => RunChanges.fromJson(
    await _json('GET', '/v1/runs/${_seg(runId)}/changes'),
  );

  /// Put a run's folder back as its snapshot had it (the changes it
  /// reverted). The caller lists [runChanges] and confirms first.
  Future<RunChanges> undoRun(String runId) async => RunChanges.fromJson(
    await _json('POST', '/v1/runs/${_seg(runId)}/undo', {'confirm': true}),
  );

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

  /// Rewrite a memory; it becomes yours (`source: user`).
  Future<Memory> editMemory(int id, String content) async => Memory.fromJson(
    await _json('PATCH', '/v1/memories/$id', {'content': content}),
  );

  Future<void> deleteMemory(int id) => _json('DELETE', '/v1/memories/$id');

  /// Delete every memory of one namespace: `shared` or an agent.
  Future<int> clearMemories(String agent) async {
    final r = await _json(
      'DELETE',
      '/v1/memories?agent=${Uri.encodeQueryComponent(agent)}',
    );
    return (r as Json)['deleted'] as int? ?? 0;
  }

  Future<List<Channel>> channels({bool archived = false}) async => [
    for (final c
        in await _json('GET', '/v1/channels${archived ? '?archived=1' : ''}')
            as List)
      Channel.fromJson(c as Json),
  ];

  /// A topic channel. Throws [TernException] (409) if the slug is taken.
  ///
  /// [workspace] (a workspace id or a folder path) binds the channel's folder
  /// at creation, so its first run already uses it. A folder Tern refuses
  /// throws [TernException] with status 422 and the reason.
  Future<Channel> createChannel({
    required String slug,
    String? title,
    String? instructions,
    String? workspace,
  }) async => Channel.fromJson(
    await _json('POST', '/v1/channels', {
      'slug': slug,
      'title': ?title,
      'instructions': ?instructions,
      'workspace': ?workspace,
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
  ///
  /// [workspace] (a workspace id or folder path) is the folder this
  /// conversation should use: a topic channel is bound to it from this run;
  /// in Home a folder other than the topic's current one starts a new topic
  /// in it (`Sent.newSegment`), so the first run already uses it.
  Future<Sent> sendToChannel(
    String channel,
    String text, {
    int? replyTo,
    bool newTopic = false,
    String? workspace,
  }) async => Sent.fromJson(
    await _json('POST', '/v1/channels/${_seg(channel)}/posts', {
      'text': text,
      'reply_to': ?replyTo,
      if (newTopic) 'new_topic': true,
      'workspace': ?workspace,
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

  /// A session and its messages. With `limit`, the newest that many (those
  /// before message index `before` when given), so a long history loads in
  /// pages; a daemon that predates paging sends the whole session.
  Future<SessionDetail> session(String id, {int? limit, int? before}) async {
    final query = [
      if (limit != null) 'limit=$limit',
      if (before != null) 'before=$before',
    ].join('&');
    return SessionDetail.fromJson(
      await _json('GET', '/v1/sessions/$id${query.isEmpty ? '' : '?$query'}'),
    );
  }

  Future<void> setModel(String session, String model) =>
      _json('PUT', '/v1/sessions/$session/model', {'model': model});

  Future<Usage> usage(String session) async =>
      Usage.fromJson(await _json('GET', '/v1/sessions/$session/usage'));

  /// Send a message. Without `session` the daemon starts a new
  /// conversation. A run already in progress in that session is cancelled
  /// first. (Conversations in a channel go through [sendToChannel].)
  Future<Started> send(
    String text, {
    String? session,
    String? model,
    String? workspace,
  }) async => Started.fromJson(
    await _json('POST', '/v1/runs', {
      'text': text,
      'session_id': ?session,
      'model': ?model,
      'workspace': ?workspace,
    }),
  );

  // ---- workspaces (docs/alpha-workspaces.md) ----

  /// Every folder the agent can reach, the default first. [archived] adds
  /// removed ones. The sandbox check isn't waited for here, so `shellState`
  /// may read `unverified` until it lands; [workspace] waits.
  Future<List<Workspace>> workspaces({bool archived = false}) async => [
    for (final w
        in await _json('GET', '/v1/workspaces${archived ? '?archived=1' : ''}')
            as List)
      Workspace.fromJson(w as Json),
  ];

  Future<Workspace> workspace(String id) async => Workspace.fromJson(
    await _json('GET', '/v1/workspaces/${_seg(id)}') as Json,
  );

  /// Judge a folder without saving anything: the words for the confirm
  /// sheet. [readOnly] judges it as a read-only extra folder or file.
  Future<WorkspaceVerdict> validateWorkspace(
    String path, {
    bool readOnly = false,
  }) async => WorkspaceVerdict.fromJson(
    await _json('POST', '/v1/workspaces/validate', {
      'path': path,
      if (readOnly) 'kind': 'read_root',
    }),
  );

  /// Add a folder (one already added is returned). Throws
  /// [WorkspaceRefused] with the reason when Tern won't use it.
  Future<Workspace> addWorkspace(String path, {String? name}) async {
    try {
      final j =
          await _json('POST', '/v1/workspaces', {'path': path, 'name': ?name})
              as Json;
      return Workspace.fromJson(j['workspace'] as Json);
    } on TernException catch (e) {
      throw _refusal(e);
    }
  }

  /// Rename it, or set its network (`'off'`; null inherits).
  Future<Workspace> updateWorkspace(
    String id, {
    String? name,
    String? network,
    bool clearNetwork = false,
  }) async => Workspace.fromJson(
    await _json('PATCH', '/v1/workspaces/${_seg(id)}', {
      'name': ?name,
      if (clearNetwork) 'network': null else 'network': ?network,
    }),
  );

  /// Point a missing folder at its new place (Locate). The same folder is
  /// accepted at once; another needs [confirm], else this throws
  /// [TernException] with status 409 and `body['needs_confirm'] == true`.
  Future<Workspace> locateWorkspace(
    String id,
    String path, {
    bool confirm = false,
  }) async => Workspace.fromJson(
    await _json('PATCH', '/v1/workspaces/${_seg(id)}', {
      'locate': path,
      if (confirm) 'confirm': true,
    }),
  );

  /// Re-check a folder now; the result has its status.
  Future<Workspace> checkWorkspace(String id) async => Workspace.fromJson(
    await _json('POST', '/v1/workspaces/${_seg(id)}/check') as Json,
  );

  /// Remove a folder from Tern (its files are never touched). Throws with
  /// status 409 (and `body['impact']`) while something uses it, unless
  /// [reassignTo] names the workspace to move those to.
  Future<void> removeWorkspace(String id, {String? reassignTo}) => _json(
    'DELETE',
    '/v1/workspaces/${_seg(id)}'
        '${reassignTo == null ? '' : '?reassign_to=${Uri.encodeQueryComponent(reassignTo)}'}',
  );

  /// The read-only extra folders (and pinned files) of a workspace.
  Future<List<WorkspaceRoot>> workspaceRoots(String id) async => [
    for (final r
        in await _json('GET', '/v1/workspaces/${_seg(id)}/roots') as List)
      WorkspaceRoot.fromJson(r as Json),
  ];

  /// Let the agent read [path] (a folder or one file), never change it.
  /// Throws [WorkspaceRefused] when Tern won't allow it.
  Future<WorkspaceRoot> addWorkspaceRoot(String id, String path) async {
    try {
      final j =
          await _json('POST', '/v1/workspaces/${_seg(id)}/roots', {
                'path': path,
              })
              as Json;
      return WorkspaceRoot.fromJson(j['root'] as Json);
    } on TernException catch (e) {
      throw _refusal(e);
    }
  }

  Future<void> removeWorkspaceRoot(String id, String path) => _json(
    'DELETE',
    '/v1/workspaces/${_seg(id)}/roots?path=${Uri.encodeQueryComponent(path)}',
  );

  /// The folder a channel uses, where it comes from, and its own binding.
  Future<WorkspaceBinding> channelWorkspace(String channel) async =>
      WorkspaceBinding.fromJson(
        await _json('GET', '/v1/channels/${_seg(channel)}/workspace') as Json,
      );

  /// Bind a channel to [workspace] (an id or a folder path); null clears
  /// (inherit the agent's, then the default). Applies from its next run.
  Future<WorkspaceBinding> setChannelWorkspace(
    String channel,
    String? workspace,
  ) => _bindWorkspace('channels', channel, workspace);

  Future<WorkspaceBinding> sessionWorkspace(String session) async =>
      WorkspaceBinding.fromJson(
        await _json('GET', '/v1/sessions/${_seg(session)}/workspace') as Json,
      );

  /// Bind one conversation (a Home topic). For a new Home topic in another
  /// folder, send with `workspace:` instead, which starts the topic there.
  Future<WorkspaceBinding> setSessionWorkspace(
    String session,
    String? workspace,
  ) => _bindWorkspace('sessions', session, workspace);

  Future<WorkspaceBinding> agentWorkspace(String agent) async =>
      WorkspaceBinding.fromJson(
        await _json('GET', '/v1/agents/${_seg(agent)}/workspace') as Json,
      );

  /// An agent's default folder (used where its channel and topic set none).
  Future<WorkspaceBinding> setAgentWorkspace(String agent, String? workspace) =>
      _bindWorkspace('agents', agent, workspace);

  Future<WorkspaceBinding> _bindWorkspace(
    String kind,
    String id,
    String? workspace,
  ) async {
    try {
      return WorkspaceBinding.fromJson(
        await _json('PUT', '/v1/$kind/${_seg(id)}/workspace', {
              'workspace': workspace,
            })
            as Json,
      );
    } on TernException catch (e) {
      throw _refusal(e);
    }
  }

  /// A 422 with a folder verdict becomes [WorkspaceRefused]; anything else
  /// passes through.
  Object _refusal(TernException e) {
    final v = e.body?['verdict'];
    if (e.status == 422 && v is Json) {
      return WorkspaceRefused(WorkspaceVerdict.fromJson(v), e.message);
    }
    return e;
  }

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
      if (res.statusCode >= 300) {
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
    if (res.statusCode >= 300) {
      final msg = decoded is Json ? decoded['error'] as String? : null;
      final err = _error(res.statusCode, msg);
      throw decoded is Json && err.runtimeType == TernException
          ? TernException(err.message, status: err.status, body: decoded)
          : err;
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
    if (status >= 300 && status < 400) {
      return TernException(
        'the server at ${endpoint.describe()} redirected the request '
        '(HTTP $status); not followed, so the device token stays with the '
        'address it was paired with',
        status: status,
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
      // A server that answers with a redirect isn't followed: the token is
      // for the address that was paired, never for wherever it points.
      if (endpoint is RemoteEndpoint) req.followRedirects = false;
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
