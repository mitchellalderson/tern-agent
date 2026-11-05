// Switching servers rebuilds the app on a fresh store: the old one is shut
// down, its selection and caches are gone, and the new client carries only
// the new server's token.
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/main.dart';
import 'package:tern_desktop/state/connection.dart';
import 'package:tern_desktop/state/daemon.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/ui/home.dart';

import 'servers_test.dart' show MemTokens, settle;

void main() {
  testWidgets('a switch gives a new store, shuts the old one down, and keeps '
      'tokens apart', (tester) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
    final dir = Directory.systemTemp.createTempSync('tern_switch');
    addTearDown(() => dir.deleteSync(recursive: true));
    final prefs = AppPrefs(File('${dir.path}/desktop.json'));
    final tokens = MemTokens();
    final work = RemoteProfile(
      url: Uri.parse('https://work.example.net:7420'),
      name: 'Work box',
      pin: 'sha256:${'AB' * 32}',
    );
    final home = RemoteProfile(
      url: Uri.parse('https://home.example.net:7420'),
      name: 'Home server',
    );
    prefs.servers.addAll([work, home]);
    prefs.connection = work;
    tokens.tokens[work.account] = 'tok-work';
    tokens.tokens[home.account] = 'tok-home';
    prefs.read['general'] = 7;

    await tester.pumpWidget(
      TernApp(
        prefs: prefs,
        tokens: tokens,
        endpoint: Endpoint.remote(work.url, token: 'tok-work', pin: work.pin),
      ),
    );
    await tester.pump();

    Home current() => tester.widget<Home>(find.byType(Home));
    final connections = current().connections!;
    final first = current().store;
    expect((first.client.endpoint as RemoteEndpoint).token, 'tok-work');
    expect(first.lastSeen, {'general': 7});
    // Something that belongs to the first server's view.
    first.selected = 'session-on-work-box';

    await tester.runAsync(() => connections.switchTo(home.id));
    await settle(tester);

    final second = current().store;
    expect(second, isNot(same(first)));
    final e = second.client.endpoint as RemoteEndpoint;
    expect(
      (e.base.host, e.token, e.pin),
      ('home.example.net', 'tok-home', null),
    );
    // Nothing carried over: not the selection, not the unread marks.
    expect(second.selected, isNot('session-on-work-box'));
    expect(second.lastSeen, isEmpty);
    // The old store is shut down for good.
    expect(() => first.addListener(() {}), throwsFlutterError);
    expect(connections.activeId, home.id);

    // And back: the first server's marks are still its own.
    await tester.runAsync(() => connections.switchTo(work.id));
    await settle(tester);
    final third = current().store;
    expect((third.client.endpoint as RemoteEndpoint).token, 'tok-work');
    expect(third.lastSeen, {'general': 7});
    expect(third, isA<TernStore>());
    debugDefaultTargetPlatformOverride = null;
  });
}
