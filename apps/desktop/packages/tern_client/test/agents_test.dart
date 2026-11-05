import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

void main() {
  test('an agent as GET /v1/agents sends it', () {
    final a = Agent.fromJson({
      'id': 'tern:research',
      'slug': 'research',
      'name': 'Research',
      'avatar': {'glyph': '★', 'color': 'blue'},
      'model': ['mock:down', 'mock:up'],
      'subagent_model': null,
      'tools': {
        'builtins': 'read_only',
        'mcp': ['browser'],
      },
      'memory': 'private',
      'delegates': <String>[],
      'main_channel_id': 'dm:research',
      'last_activity': 1762185600000,
      'needs_you': 1,
      'active_runs': 0,
      'updated_at': 1762185600000,
    });
    expect(a.model, ['mock:down', 'mock:up']);
    expect(a.isDefault, isFalse);
    expect(a.privateMemory, isTrue);
    expect(a.tools!.builtins.readOnly, isTrue);
    expect(a.tools!.connectors.all, isTrue);
    expect(a.tools!.mcp.allows('browser'), isTrue);
    expect(a.tools!.toJson()['mcp'], ['browser']);
    final tern = Agent.fromJson({
      'id': 'tern',
      'slug': 'tern',
      'name': 'tern',
      'model': 'mock:m',
      'memory': 'shared',
      'main_channel_id': 'home',
      'is_default': true,
      'updated_at': 0,
    });
    expect(tern.isDefault, isTrue);
    expect(tern.model, ['mock:m']);
  });

  test('a memory carries where it came from', () {
    final base = {
      'id': 3,
      'content': 'Acme renews in March',
      'tags': ['acme'],
      'created_at': 1762185600000,
      'agent_id': 'tern:research',
    };
    final m = Memory.fromJson(base);
    expect((m.source, m.unverified, m.shared), ('agent', false, false));
    final bad = Memory.fromJson({...base, 'source': 'unverified'});
    expect(bad.unverified, isTrue);
    expect(Memory.fromJson({...base, 'source': 'user'}).unverified, isFalse);
  });

  test('templates, approvals and channels carry their agent', () {
    final t = AgentTemplate.fromJson({
      'id': 'inbox',
      'name': 'Inbox',
      'description': 'd',
      'persona': 'p',
      'category': 'Everyday',
      'tools': null,
      'memory': 'private',
      'autonomy': 'supervised',
      'routine': ['0 8 * * 1-5', 'Check my inbox.'],
      'avatar': ['■', 'amber'],
    });
    expect(t.category, 'Everyday');
    expect(t.autonomy, 'supervised');
    expect(t.autonomyLevel, AutonomyLevel.supervised);
    expect(t.searchText, contains('everyday'));
    expect(t.routine?.prompt, 'Check my inbox.');
    expect(t.avatar?.color, 'amber');
    final ev = TernEvent.fromJson({
      'approval_requested': {
        'id': 'a',
        'run_id': 'r',
        'session_id': 's',
        'tool': 'write_file',
        'input': {},
        'summary': 'x',
        'pattern': null,
        'channel_id': 'dm:research',
        'agent_id': 'tern:research',
      },
    });
    expect((ev as ApprovalRequested).approval.channelId, 'dm:research');
    expect(TernEvent.fromJson({'agents_changed': {}}), isA<AgentsChanged>());
    final dm = Channel.fromJson({
      'id': 'dm:research',
      'slug': 'dm-research',
      'title': 'Research',
      'kind': 'main',
      'agent_id': 'tern:research',
      'updated_at': 0,
    });
    expect((dm.isMain, dm.isHome, dm.isDm), (true, false, true));
    // A core before agents: its one main channel is Home.
    final old = Channel.fromJson({
      'id': 'home',
      'slug': 'home',
      'title': 'Home',
      'kind': 'main',
      'updated_at': 0,
    });
    expect(old.isHome, isTrue);
  });

  test('impacts of deleting an agent or a channel, and deleted agents', () {
    final a = AgentImpact.fromJson({
      'id': 'tern:research',
      'slug': 'research',
      'name': 'Research',
      'routines': [
        {'id': 'r1', 'label': 'morning brief'},
      ],
      'led_channels': [
        {'id': 'c1', 'label': 'launch'},
      ],
      'member_channels': <Object>[],
      'conversations': 4,
      'memories': 2,
      'running_runs': 1,
      'pending_outbox': 0,
      'blockers': ['1 run(s) of @research are in progress'],
    });
    expect(a.routines.single.label, 'morning brief');
    expect(a.ledChannels.single.label, 'launch');
    expect((a.conversations, a.memories, a.runningRuns), (4, 2, 1));
    expect(a.blockers, hasLength(1));
    final c = ChannelImpact.fromJson({
      'id': 'c1',
      'slug': 'launch',
      'title': 'Launch',
      'posts': 12,
      'conversations': 3,
      'routines': <Object>[],
      'members': 1,
      'running_runs': 0,
      'blockers': <String>[],
    });
    expect((c.posts, c.conversations), (12, 3));
    expect(c.blockers, isEmpty);
    final gone = Agent.fromJson({
      'id': 'tern:old',
      'slug': '~tern:old',
      'name': 'Old',
      'memory': 'private',
      'archived': true,
      'deleted': true,
      'updated_at': 0,
    });
    expect((gone.deleted, gone.archived), (true, true));
  });

  test('an agent carries its autonomy ceiling and its own bot', () {
    final a = Agent.fromJson({
      'id': 'tern:ops',
      'slug': 'ops',
      'name': 'Ops',
      'memory': 'private',
      'autonomy': 'supervised',
      'telegram': {'bot_token_secret': 'OPS_BOT'},
      'updated_at': 0,
    });
    expect(a.autonomy, 'supervised');
    expect(a.autonomyLevel, AutonomyLevel.supervised);
    expect(a.telegramSecret, 'OPS_BOT');
    final none = Agent.fromJson({
      'id': 'tern:x',
      'slug': 'x',
      'name': 'X',
      'updated_at': 0,
    });
    expect(none.autonomy, isNull);
    expect(none.autonomyLevel, isNull);
    expect(none.telegramSecret, isNull);
    // The core still accepts `read_only`; the client says `readonly`.
    final old = Agent.fromJson({
      'id': 'tern:y',
      'slug': 'y',
      'name': 'Y',
      'autonomy': 'read_only',
      'updated_at': 0,
    });
    expect(old.autonomy, 'readonly');
    expect(AutonomyLevel.values.map((l) => l.wire), [
      'readonly',
      'supervised',
      'code',
      'autonomous',
      'yolo',
    ]);
  });

  test('a topic channel says whether agents decide', () {
    expect(ChannelAgents.fromJson({'members': []}).letAgentsDecide, isFalse);
    expect(
      ChannelAgents.fromJson({
        'members': [],
        'let_agents_decide': true,
      }).letAgentsDecide,
      isTrue,
    );
  });
}
