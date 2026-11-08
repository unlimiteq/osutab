using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using OpenTabletDriver.Plugin.Devices;

namespace OsutabBridge
{
    /// <summary>
    /// Presents the S Pen as a device. The matching tablet configuration names these IDs, from
    /// pid.codes' range for testing, so it can't be mistaken for any real tablet.
    /// </summary>
    public class OsutabEndpoint : IDeviceEndpoint
    {
        public const int Vendor = 0x1209;
        public const int Product = 0x0001;

        public int ProductID => Product;
        public int VendorID => Vendor;
        public int InputReportLength => OsutabReport.FrameLength;
        public int OutputReportLength => 0;
        public int FeatureReportLength => 0;
        public string? Manufacturer => "osu!tab";
        public string? ProductName => "S Pen through osu!tab";
        public string? FriendlyName => ProductName;
        public string? SerialNumber => "osutab";
        public string DevicePath => "osutab://s-pen";
        public bool CanOpen => true;
        public IDictionary<string, string> DeviceAttributes { get; } = new Dictionary<string, string>();

        public IDeviceEndpointStream Open() => new OsutabStream();
        public string? GetDeviceString(byte index) => ProductName;
        public void OpenAndSendFeatureInit(byte[] initData) { }
        public void OpenAndSendOutputInit(byte[] initData) { }
    }

    /// <summary>
    /// Reads pen reports from osu!tab on this PC. It keeps trying to connect, because osu!tab can
    /// start after OpenTabletDriver, and it reconnects whenever osu!tab restarts.
    /// </summary>
    public class OsutabStream : IDeviceEndpointStream
    {
        private const int Port = 27193;
        private const int RetryMs = 200;

        private TcpClient? client;
        private NetworkStream? stream;
        private volatile bool disposed;

        public byte[] Read()
        {
            byte[] frame = new byte[OsutabReport.FrameLength];
            while (true)
            {
                if (disposed)
                    throw new ObjectDisposedException(nameof(OsutabStream));
                try
                {
                    stream ??= Connect();
                    stream.ReadExactly(frame);
                    return frame;
                }
                catch (Exception e) when (e is IOException or SocketException or EndOfStreamException)
                {
                    CloseConnection();
                    Thread.Sleep(RetryMs);
                }
            }
        }

        private NetworkStream Connect()
        {
            client = new TcpClient { NoDelay = true };
            client.Connect(IPAddress.Loopback, Port);
            return client.GetStream();
        }

        private void CloseConnection()
        {
            stream?.Dispose();
            client?.Dispose();
            stream = null;
            client = null;
        }

        public void Write(byte[] buffer) { }
        public void GetFeature(byte[] buffer) { }
        public void SetFeature(byte[] buffer) { }

        public void Dispose()
        {
            disposed = true;
            CloseConnection();
        }
    }

    public class OsutabHub : IDeviceHub
    {
        private IDeviceEndpoint[] devices = Array.Empty<IDeviceEndpoint>();

        public event EventHandler<DevicesChangedEventArgs>? DevicesChanged;

        public IEnumerable<IDeviceEndpoint> GetDevices() => devices;

        /// <summary>
        /// Adds the S Pen. This has to happen after the hub is connected: RootHub's
        /// ConnectDeviceHub reports a new hub's devices as removals, so the daemon wouldn't run
        /// detection for a device that was present at that point.
        /// </summary>
        public void AnnounceDevice()
        {
            if (devices.Length > 0)
                return;
            var previous = devices;
            devices = new IDeviceEndpoint[] { new OsutabEndpoint() };
            DevicesChanged?.Invoke(this, new DevicesChangedEventArgs(previous, devices));
        }
    }
}
