// A run's lifecycle in Home: busy while it works, idle once it finishes,
// whatever its sub-agents (and delegates to other profiles) did; Stop and a
// resync clear state the daemon no longer has.
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';

final _t0 = DateTime(2026, 11, 4, 9);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  /// What `/v1/runs/active` says.
  var active = <ActiveRun>[];

  /// `h1` was moved to #market; Home's stream is a fresh `h2`.
  var moved = false;

  @override
  Future<MoveResult> moveToChannel(
    List<String> sessions, {
    String? channel,
    String? newChannelTitle,
  }) async {
    moved = true;
    return MoveResult(channel: (await channels()).last, moved: const []);
  }

  @override
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async => [];
  final cancelled = <String>[];

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      agentId: 'tern',
      mainSessionId: moved ? 'h2' : 'h1',
      updatedAt: _t0,
    ),
    if (moved)
      Channel(
        id: 'market',
        slug: 'market',
        title: 'Market',
        kind: 'topic',
        mainSessionId: 'h1',
        updatedAt: _t0,
      ),
    Channel(
      id: 'dm:research',
      slug: 'dm-research',
      title: 'Research',
      kind: 'main',
      agentId: 'tern:research',
      updatedAt: _t0,
    ),
  ];

  @override
  Future<List<Agent>> agents({bool archived = false}) async => const [
    Agent(
      id: 'tern',
      slug: 'tern',
      name: 'tern',
      memory: 'shared',
      mainChannelId: 'home',
      isDefault: true,
    ),
    Agent(
      id: 'tern:research',
      slug: 'research',
      name: 'Research',
      mainChannelId: 'dm:research',
    ),
  ];

  @override
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async => StreamPage(
    items: [
      if (moved)
        StreamCut(sessionId: 'h2', reason: 'new_topic', createdAt: _t0)
      else
        StreamMessage(
          sessionId: 'h1',
          messageId: 1,
          message: const SessionMessage(
            role: 'user',
            content: [TextBlock('Research the market for us')],
            summary: false,
          ),
          createdAt: _t0,
        ),
    ],
  );

  @override
  Future<List<ActiveRun>> activeRuns() async => active;

  @override
  Future<bool> cancel(String run) async {
    cancelled.add(run);
    return active.any((r) => r.runId == run);
  }

  @override
  Future<Usage> usage(String session) async => const Usage();

  @override
  Future<List<SessionItem>> sessions({int limit = 50, String? channel}) async =>
      [];

  @override
  Future<List<Schedule>> schedules() async => [];

  @override
  Future<List<ChildRun>> subagents(String session) async => [];
}

Future<(TernStore, _FakeClient)> _home() async {
  final client = _FakeClient();
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(
      pid: 1,
      version: '0.2.0',
      features: {'channels', 'agents'},
    )
    ..info = const Info(defaultModel: 'mock:m', tools: []);
  store.channels = await client.channels();
  store.agents = await client.agents();
  await store.openHome();
  return (store, client);
}

/// An event as the daemon sends it: with the run's session, channel and
/// agent, and a sub-agent's parent and root.
void _event(TernStore store, String type, String run, Map<String, Object?> j) {
  final child = run.startsWith('k');
  store.onEventForTest(
    TernEvent.fromJson({
      'type': type,
      'run_id': run,
      'session_id': child ? 's-$run' : 'h1',
      'channel_id': 'home',
      'agent_id': child ? 'tern:research' : 'tern',
      if (child) 'parent_run_id': 'r1',
      if (child) 'root_session_id': 'h1',
      ...j,
    }),
  );
}

/// The parent delegating four tasks to Research, all of them finishing,
/// and the parent answering. `childrenLast`: the children's
/// `run_finished` arrive after the parent's (the event stream doesn't
/// promise an order across runs).
void _delegation(TernStore store, {bool childrenLast = false}) {
  final kids = ['k1', 'k2', 'k3', 'k4'];
  _event(store, 'run_started', 'r1', {});
  for (final (i, k) in kids.indexed) {
    _event(store, 'tool_started', 'r1', {
      'id': 'c$i',
      'name': 'agent_spawn',
      'summary': 'agent_spawn market-$i',
    });
    _event(store, 'subagent_spawned', 'r1', {
      'call_id': 'c$i',
      'child_run_id': k,
      'child_session_id': 's-$k',
      'label': 'market-$i',
      'task': 'Look at segment $i',
      'agent': 'research',
    });
    _event(store, 'tool_finished', 'r1', {
      'id': 'c$i',
      'name': 'agent_spawn',
      'output': '{"run_id":"$k","status":"running"}',
    });
    _event(store, 'run_started', k, {});
    _event(store, 'tool_started', k, {
      'id': 't$i',
      'name': 'web_search',
      'summary': 'web_search segment $i',
    });
  }
  _event(store, 'tool_started', 'r1', {
    'id': 'w',
    'name': 'agent_wait',
    'summary': 'agent_wait',
  });
  void finishKids() {
    for (final (i, k) in kids.indexed) {
      _event(store, 'tool_finished', k, {'id': 't$i', 'name': 'web_search'});
      _event(store, 'assistant_message', k, {'text': 'Segment $i: growing.'});
      _event(store, 'run_finished', k, {'status': 'completed'});
    }
  }

  if (!childrenLast) finishKids();
  _event(store, 'tool_finished', 'r1', {'id': 'w', 'name': 'agent_wait'});
  _event(store, 'assistant_message', 'r1', {
    'text': 'All four segments are growing. What next?',
  });
  _event(store, 'run_finished', 'r1', {'status': 'completed'});
  if (childrenLast) finishKids();
}

void main() {
  test('Home is idle once a parent that delegated finishes', () async {
    final (store, _) = await _home();
    _delegation(store);
    expect(store.running, isFalse);
    expect(store.current!.activeRun, isNull);
    expect(store.busy(store.channels.first), isFalse);
    expect(store.current!.phase, isNull);
  });

  test('children finishing after their parent leave Home idle', () async {
    final (store, _) = await _home();
    _delegation(store, childrenLast: true);
    expect(store.running, isFalse);
    expect(store.current!.phase, isNull);
  });

  test('a child event placed in Home never makes Home busy', () async {
    final (store, _) = await _home();
    _event(store, 'run_started', 'r1', {});
    // A delegate's run that starts in a session the stream holds (as a
    // Home thread would): it must not take over Home's run.
    store.onEventForTest(
      TernEvent.fromJson({
        'type': 'run_started',
        'run_id': 'k9',
        'session_id': 'h1',
        'channel_id': 'home',
        'agent_id': 'tern:research',
        'parent_run_id': 'r1',
      }),
    );
    expect(store.current!.activeRun, 'r1');
    _event(store, 'run_finished', 'r1', {'status': 'completed'});
    expect(store.running, isFalse);
  });

  test('Stop on a run the daemon already finished clears it', () async {
    final (store, client) = await _home();
    _event(store, 'run_started', 'r1', {});
    expect(store.running, isTrue);
    // Its run_finished never arrived; the daemon has nothing running.
    await store.stop();
    expect(client.cancelled, ['r1']);
    expect(store.running, isFalse);
    expect(store.error, isNull);
  });

  test('Stop on a run that is still working leaves it to finish', () async {
    final (store, client) = await _home();
    _event(store, 'run_started', 'r1', {});
    client.active = [const ActiveRun(runId: 'r1', sessionId: 'h1')];
    await store.stop();
    expect(store.running, isTrue, reason: 'run_finished will clear it');
  });

  test(
    'a reply that arrives after its run finished does not revive it',
    () async {
      final (store, _) = await _home();
      _event(store, 'run_started', 'r1', {});
      _event(store, 'run_finished', 'r1', {'status': 'completed'});
      // The send's HTTP answer, or an active-runs list, landing late.
      store.current!.activeRun ??= 'r1';
      expect(store.running, isFalse);
    },
  );

  test('the daemon saying it dropped events resyncs', () async {
    final (store, client) = await _home();
    _event(store, 'run_started', 'r1', {});
    store.onEventForTest(
      TernEvent.fromJson({
        'events_dropped': {'missed': 40},
      }),
    );
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(Duration.zero);
    expect(store.running, isFalse);
    expect(client.cancelled, isEmpty);
  });

  test('a resync clears runs the daemon no longer has', () async {
    final (store, client) = await _home();
    _event(store, 'run_started', 'r1', {});
    client.active = [const ActiveRun(runId: 'r2', sessionId: 's-other')];
    await store.resyncRuns();
    expect(store.running, isFalse);
    expect(store.busy(store.channels.first), isFalse);
    expect(store.transcripts['s-other']!.activeRun, 'r2');
  });

  for (final real in [false, true]) {
    test(
      'moving a ${real ? 'working' : 'stale'} session leaves Home idle',
      () async {
        final (store, client) = await _home();
        _event(store, 'run_started', 'r1', {});
        if (real) {
          client.active = [
            const ActiveRun(runId: 'r1', sessionId: 'h1', channelId: 'market'),
          ];
        }
        expect(await store.moveToChannel(['h1'], newTitle: 'Market'), isNull);
        await Future<void>.delayed(Duration.zero);
        await store.openHome();
        expect(store.running, isFalse, reason: "Home's new segment is idle");
        expect(store.transcripts['h1']?.activeRun, real ? 'r1' : null);
      },
    );
  }
}
