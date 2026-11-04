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

  test('templates, approvals and channels carry their agent', () {
    final t = AgentTemplate.fromJson({
      'id': 'inbox',
      'name': 'Inbox',
      'description': 'd',
      'persona': 'p',
      'category': 'Everyday',
      'tools': null,
      'memory': 'private',
      'routine': ['0 8 * * 1-5', 'Check my inbox.'],
      'avatar': ['■', 'amber'],
    });
    expect(t.category, 'Everyday');
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
}
