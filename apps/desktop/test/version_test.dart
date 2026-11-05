import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_desktop/state/version.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/version_banner.dart';

void main() {
  group('coreSkewMessage', () {
    test('matching versions are quiet', () {
      expect(
        coreSkewMessage(appVersion: '0.2.0', coreVersion: '0.2.0', local: true),
        isNull,
      );
    });

    test('a different local core names both versions', () {
      final m = coreSkewMessage(
        appVersion: '0.2.0',
        coreVersion: '0.1.0',
        local: true,
      );
      expect(m, contains('v0.2.0'));
      expect(m, contains('v0.1.0'));
      expect(m, contains('Restart tern'));
    });

    test('a core on a server is not ours to compare', () {
      expect(
        coreSkewMessage(
          appVersion: '0.2.0',
          coreVersion: '0.1.0',
          local: false,
        ),
        isNull,
      );
    });

    test('a development build has nothing to compare', () {
      expect(
        coreSkewMessage(appVersion: '', coreVersion: '0.1.0', local: true),
        isNull,
      );
      expect(appVersionLabel(''), 'development build');
      expect(appVersionLabel('0.2.0'), '0.2.0');
    });
  });

  testWidgets('the banner restarts tern and shows a failure', (tester) async {
    var restarts = 0;
    var fail = false;
    await tester.pumpWidget(
      MaterialApp(
        theme: ternTheme(TernColors.light, Brightness.light),
        home: Scaffold(
          body: VersionBanner(
            message: 'Versions differ.',
            onRestart: () async {
              restarts++;
              if (fail) throw StateError('no tern');
            },
          ),
        ),
      ),
    );
    expect(find.text('Versions differ.'), findsOneWidget);
    await tester.tap(find.text('Restart tern'));
    await tester.pump();
    expect(restarts, 1);
    fail = true;
    await tester.tap(find.text('Restart tern'));
    await tester.pump();
    expect(find.textContaining('Restart failed'), findsOneWidget);
  });
}
