import Foundation
import Testing
@testable import Flotilla

/// Send later's time of day: every minute of the day comes back as it went in, in zones whose
/// clocks skip an hour, half an hour or midnight, whatever today is. (Held on today's date, a
/// 02:30 opened on a day the clocks skip it came back as 03:30 and was saved so.)
@Suite struct ScheduleSheetClock {
    func calendar(_ zone: String) -> Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: zone)!
        return c
    }

    @Test func `every time of day survives the picker in zones with changes of the clocks`() {
        for zone in ["America/New_York", "Europe/London", "Australia/Lord_Howe", "America/Santiago", "Pacific/Apia", "Asia/Kathmandu"] {
            let cal = calendar(zone)
            for at in 0..<(24 * 60) {
                #expect(ScheduleSheet.minutes(ScheduleSheet.clockDate(at, calendar: cal), calendar: cal) == at, "\(zone) \(at)")
            }
        }
    }

    /// An every-N-hours schedule from before opens as daily at its first run's time in its
    /// zone: 02:30 on 1 March 2026 in New York, a time that day has (8 March doesn't).
    @Test func `an old every N hours schedule opens at its first run's time`() {
        let cal = calendar("America/New_York")
        let first = cal.date(from: DateComponents(year: 2026, month: 3, day: 1, hour: 2, minute: 30))!
        let at = ScheduleSheet.minutes(first, calendar: cal)
        #expect(at == 150)
        #expect(ScheduleSheet.minutes(ScheduleSheet.clockDate(at, calendar: cal), calendar: cal) == 150)
        // As it was held before, on the day it was opened: on 8 March 2026 it didn't survive.
        let march8 = cal.date(from: DateComponents(year: 2026, month: 3, day: 8, hour: 12))!
        #expect(cal.date(bySettingHour: 2, minute: 30, second: 0, of: march8).map { ScheduleSheet.minutes($0, calendar: cal) } != 150)
    }
}
