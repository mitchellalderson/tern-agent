// A follow-up the agent promised reads differently from a plain wait.
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/ui/wait_strip.dart';

void main() {
  test('a promised follow-up says when, a plain wait says waiting', () {
    final due = DateTime(2026, 11, 21, 9); // a Saturday
    final promised = Wait.fromJson({
      'id': 'w1',
      'note': 'Check whether Riley replied',
      'agent_id': 'tern',
      'session_id': 's1',
      'expires_at': due.millisecondsSinceEpoch,
      'followup': true,
    });
    expect(promised.followup, isTrue);
    expect(waitLabel(promised), 'Following up Sat: Check whether Riley replied');
    final plain = Wait.fromJson({
      'id': 'w2',
      'note': 'CI to finish',
      'agent_id': 'tern',
      'session_id': 's1',
    });
    expect(plain.followup, isFalse);
    expect(waitLabel(plain), 'Waiting: CI to finish');
  });

  test('a tidy-up entry reads its memories', () {
    final t = MemoryTidy.fromJson({
      'id': 3,
      'at': 1000,
      'action': 'merge',
      'namespace': 'shared',
      'note': 'tea',
      'state': 'applied',
      'memories': [
        {'id': 1, 'content': 'A', 'source': 'agent'},
        {'id': 2, 'content': 'B', 'source': 'agent'},
      ],
    });
    expect(t.isMerge, isTrue);
    expect(t.applied, isTrue);
    expect(t.memories, ['A', 'B']);
  });
}
