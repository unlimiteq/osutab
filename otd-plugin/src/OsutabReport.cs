using System;
using System.Numerics;
using OpenTabletDriver.Plugin.Tablet;

namespace OsutabBridge
{
    /// <summary>One pen report, decoded from the 32-byte frame osu!tab sends.</summary>
    public class OsutabReport : ITabletReport, ITiltReport, IProximityReport
    {
        public const int FrameLength = 32;
        public const byte KindSample = 2;
        private const byte FlagInRange = 1 << 0;
        private const byte FlagTouch = 1 << 1;
        private const byte FlagButton = 1 << 2;

        public OsutabReport(byte[] raw)
        {
            Raw = raw;
            byte flags = raw[1];
            Position = new Vector2(BitConverter.ToUInt16(raw, 16), BitConverter.ToUInt16(raw, 18));
            // A pen that has only just touched can report no pressure yet, and a touch must still count.
            uint pressure = BitConverter.ToUInt16(raw, 20);
            Pressure = (flags & FlagTouch) != 0 ? Math.Max(pressure, 1) : 0;
            HoverDistance = raw[22];
            Tilt = new Vector2((sbyte)raw[23], (sbyte)raw[24]);
            PenButtons = new[] { (flags & FlagButton) != 0 };
            NearProximity = (flags & FlagInRange) != 0;
        }

        public byte[] Raw { get; set; }
        public Vector2 Position { get; set; }
        public uint Pressure { get; set; }
        public bool[] PenButtons { get; set; }
        public Vector2 Tilt { get; set; }
        public bool NearProximity { get; set; }
        public uint HoverDistance { get; set; }

        public static bool IsInRange(byte[] raw) => (raw[1] & FlagInRange) != 0;
    }

    /// <summary>Named in the tablet configuration, which is how OpenTabletDriver finds it.</summary>
    public class OsutabReportParser : IReportParser<IDeviceReport>
    {
        public IDeviceReport Parse(byte[] report)
        {
            if (report.Length == OsutabReport.FrameLength && report[0] == OsutabReport.KindSample && OsutabReport.IsInRange(report))
                return new OsutabReport(report);
            return new DeviceReport(report);
        }
    }
}
