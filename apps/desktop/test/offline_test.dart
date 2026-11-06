import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/offline.dart';

class _Store extends TernStore {
  _Store() : super(TernClient(const Endpoint.unix('/nonexistent/tern.sock')));
}

Future<void> _pump(
  WidgetTester tester,
  _Store store, {
  VoidCallback? onConnectRemote,
  String? bin = '/usr/bin/tern',
}) => tester.pumpWidget(
  MaterialApp(
    theme: ternTheme(TernColors.light, Brightness.light),
    home: Scaffold(
      body: OfflineView(
        store: store,
        onConnectRemote: onConnectRemote,
        locateBin: () => bin,
      ),
    ),
  ),
);

void main() {
  testWidgets('first launch is a welcome with two choices', (tester) async {
    final store = _Store()..link = Link.offline;
    store.linkError = "can't reach tern at /x: No such file or directory";
    var remote = 0;
    await _pump(tester, store, onConnectRemote: () => remote++);

    expect(find.text('Welcome to tern'), findsOneWidget);
    expect(find.text('Start on this Mac'), findsOneWidget);
    expect(find.text('Connect to a remote server…'), findsOneWidget);
    expect(find.text("tern isn't running"), findsNothing);
    expect(find.text('Try again'), findsNothing);
    expect(find.byKey(const Key('offline-details')), findsNothing);
    expect(find.textContaining('No such file'), findsNothing);

    await tester.tap(find.text('Connect to a remote server…'));
    expect(remote, 1);
  });

  testWidgets('details disclosure shows the socket and the OS error', (
    tester,
  ) async {
    final store = _Store()..link = Link.offline;
    store.linkError = "can't reach tern at /x: No such file or directory";
    await _pump(tester, store);

    await tester.tap(find.byKey(const Key('offline-details-toggle')));
    await tester.pump();
    expect(find.byKey(const Key('offline-details')), findsOneWidget);
    expect(find.textContaining('No such file'), findsOneWidget);
    expect(find.textContaining('/nonexistent/tern.sock'), findsOneWidget);

    await tester.tap(find.byKey(const Key('offline-details-toggle')));
    await tester.pump();
    expect(find.byKey(const Key('offline-details')), findsNothing);
  });

  testWidgets('without a tern binary it says how to start it', (tester) async {
    final store = _Store()..link = Link.offline;
    await _pump(tester, store, bin: null);
    expect(find.text('Start on this Mac'), findsNothing);
    expect(find.textContaining('tern start'), findsOneWidget);
  });
}
