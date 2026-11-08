using OpenTabletDriver.Plugin;
using OpenTabletDriver.Plugin.Attributes;
using OpenTabletDriver.Plugin.Components;
using OpenTabletDriver.Plugin.DependencyInjection;

namespace OsutabBridge
{
    /// <summary>
    /// Connects the S Pen to OpenTabletDriver. OpenTabletDriver only discovers device hubs in its
    /// own assembly, so this tool adds ours when the daemon starts it. Enable it once in
    /// OpenTabletDriver's Tools tab, or let osu!tab's installer do that.
    /// </summary>
    [PluginName("osu!tab bridge")]
    public class OsutabBridgeTool : ITool
    {
        // The daemon rebuilds its tools each time settings change, but the hub must stay the same
        // one: connecting it twice is ignored, and a new one would look like a new tablet.
        private static readonly OsutabHub Hub = new();

        [Resolved]
        public IDriver Driver { set; get; } = null!;

        public bool Initialize()
        {
            // The public interface doesn't expose the hub, but the driver class does.
            var property = Driver.GetType().GetProperty("CompositeDeviceHub");
            if (property?.GetValue(Driver) is not ICompositeDeviceHub composite)
            {
                Log.Write("osu!tab", "This version of OpenTabletDriver has no device hub to connect to.", LogLevel.Error);
                return false;
            }
            composite.ConnectDeviceHub(Hub);
            Hub.AnnounceDevice();
            Log.Write("osu!tab", "S Pen bridge connected.");
            return true;
        }

        // Stays connected: the daemon disposes tools on every settings change.
        public void Dispose() { }
    }
}
