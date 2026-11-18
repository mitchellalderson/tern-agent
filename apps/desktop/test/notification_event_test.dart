// The daemon's `notification` event: decoded, then raised as a system
// notification unless the window is already on that channel.
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/notifier.dart';
import 'package:tern_desktop/state/store.dart';

class _FakeNotifier implements Notifier {
  final shown = <({String id, String title, String body})>[];

  @override
  void Function(String id, NotificationAction action)? onAction;

  @override
  Future<void> show({
    required String id,
    required String title,
    required String body,
    bool approval = false,
  }) async => shown.add((id: id, title: title, body: body));

  @override
  Future<void> badge(int count) async {}
}

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));
}

void main() {
  const event = {
    'notification': {
      'title': 'Inbox sweep',
      'body': 'Three need you',
      'channel_id': 'dm:ops',
      'run_id': 'r1',
    },
  };

  test('the event decodes with and without its optional fields', () {
    final n = TernEvent.fromJson(event) as NotificationRequested;
    expect((n.title, n.body, n.channelId, n.runId), (
      'Inbox sweep',
      'Three need you',
      'dm:ops',
      'r1',
    ));
    final bare =
        TernEvent.fromJson({
              'notification': {'title': 't', 'body': 'b'},
            })
            as NotificationRequested;
    expect(bare.channelId, isNull);
    expect(bare.runId, isNull);
  });

  test('it raises a notification when the window is in the background', () {
    final notes = _FakeNotifier();
    final store = TernStore(_FakeClient(), notifier: notes)..appActive = false;
    store.onEventForTest(TernEvent.fromJson(event));
    expect(notes.shown.single.title, 'Inbox sweep');
    expect(notes.shown.single.body, 'Three need you');
    expect(notes.shown.single.id, 'channel:dm:ops');
  });

  test('it raises one when another channel is open, not the same one', () {
    final notes = _FakeNotifier();
    final store = TernStore(_FakeClient(), notifier: notes)
      ..appActive = true
      ..channel = TernStore.homeId;
    store.onEventForTest(TernEvent.fromJson(event));
    expect(notes.shown, hasLength(1));
    store.channel = 'dm:ops';
    store.onEventForTest(TernEvent.fromJson(event));
    expect(notes.shown, hasLength(1), reason: 'already looking at it');
  });
}
