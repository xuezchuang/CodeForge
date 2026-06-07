using System;
using System.IO;
using System.Runtime.Serialization.Json;
using System.Text;

namespace SnowAgent.VSBridge
{
    internal static class JsonUtil
    {
        public static T Deserialize<T>(string json)
        {
            using (var stream = new MemoryStream(Encoding.UTF8.GetBytes(json)))
            {
                var serializer = new DataContractJsonSerializer(typeof(T));
                return (T)serializer.ReadObject(stream);
            }
        }

        public static string Serialize<T>(T value)
        {
            return Serialize(value, typeof(T));
        }

        public static string Serialize(object value, Type type)
        {
            using (var stream = new MemoryStream())
            {
                var serializer = new DataContractJsonSerializer(type);
                serializer.WriteObject(stream, value);
                return Encoding.UTF8.GetString(stream.ToArray());
            }
        }
    }
}
