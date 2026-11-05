import 'dart:math' as math;

import 'package:flutter/painting.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_desktop/theme.dart';

double _lum(Color c) {
  double f(double v) =>
      v <= 0.03928 ? v / 12.92 : math.pow((v + 0.055) / 1.055, 2.4).toDouble();
  return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b);
}

double _ratio(Color a, Color b) {
  final x = _lum(a), y = _lum(b);
  return (math.max(x, y) + 0.05) / (math.min(x, y) + 0.05);
}

void main() {
  for (final (name, c) in [
    ('light', TernColors.light),
    ('dark', TernColors.dark),
  ]) {
    test('user message surface keeps AA text contrast ($name)', () {
      expect(_ratio(c.text, c.userBubble), greaterThanOrEqualTo(4.5));
    });
  }
}
