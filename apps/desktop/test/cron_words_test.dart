import 'package:flutter_test/flutter_test.dart';
import 'package:tern_desktop/state/cron_words.dart';

void main() {
  test('common schedules read as words', () {
    expect(cronInWords('0 8 * * *'), 'Daily at 8:00 AM');
    expect(cronInWords('30 17 * * 1-5'), 'Weekdays at 5:30 PM');
    expect(cronInWords('0 9 * * mon'), 'Mondays at 9:00 AM');
    expect(cronInWords('0 10 * * 0,6'), 'Weekends at 10:00 AM');
    expect(cronInWords('0 * * * *'), 'Hourly');
    expect(cronInWords('15 * * * *'), 'Hourly at :15');
    expect(cronInWords('*/10 * * * *'), 'Every 10 minutes');
    expect(cronInWords('0 0 * * *'), 'Daily at 12:00 AM');
  });

  test('anything else stays as written', () {
    expect(cronInWords('0 8 1 * *'), '0 8 1 * *');
    expect(cronInWords('0 8,20 * * *'), '0 8,20 * * *');
    expect(cronInWords('@daily'), '@daily');
  });

  test('times relative to now', () {
    final now = DateTime(2026, 11, 3, 10); // a Tuesday
    expect(whenInWords(DateTime(2026, 11, 3, 18), now), 'Today 6:00 PM');
    expect(whenInWords(DateTime(2026, 11, 4, 8), now), 'Tomorrow 8:00 AM');
    expect(whenInWords(DateTime(2026, 11, 6, 8), now), 'Fri 8:00 AM');
    expect(whenInWords(DateTime(2026, 12, 4, 8), now), 'Dec 4 8:00 AM');
  });
}
