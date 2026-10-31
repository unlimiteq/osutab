using System;
using System.Numerics;
using OpenTabletDriver.Plugin.Attributes;
using OpenTabletDriver.Plugin.Output;
using OpenTabletDriver.Plugin.Tablet;

namespace OsutabTestFilters
{
    /// <summary>
    /// For testing only. Each filter moves the pen by a known distance, so `osutab --otd-bench`
    /// can show that reports really passed through it. These are the two kinds of filter
    /// OpenTabletDriver has: one that runs per report, and one that runs on its own timer.
    /// </summary>
    [PluginName("osutab test: shift 1000 units")]
    public class ShiftFilter : IPositionedPipelineElement<IDeviceReport>
    {
        public PipelinePosition Position => PipelinePosition.PreTransform;
        public event Action<IDeviceReport>? Emit;

        public void Consume(IDeviceReport value)
        {
            if (value is ITabletReport report)
                report.Position += new Vector2(1000, 0);
            Emit?.Invoke(value);
        }
    }

    [PluginName("osutab test: async shift 2000 units")]
    public class AsyncShiftFilter : AsyncPositionedPipelineElement<IDeviceReport>
    {
        private Vector2 shifted;

        public override PipelinePosition Position => PipelinePosition.PreTransform;

        protected override void ConsumeState()
        {
            if (State is ITabletReport report)
                shifted = report.Position + new Vector2(2000, 0);
        }

        // Later pipeline stages change the report in place, and this sends the same one on every
        // tick, so it has to set the position again each time.
        protected override void UpdateState()
        {
            if (State is ITabletReport report && PenIsInRange())
            {
                report.Position = shifted;
                OnEmit();
            }
        }
    }
}
