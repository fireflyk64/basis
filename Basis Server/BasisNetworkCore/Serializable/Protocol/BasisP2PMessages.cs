using Basis.Network.Core;

public static partial class SerializableBasis
{
    public struct BasisP2PSignalMessage
    {
        public const int MaxTokenLength = 64;
        public const int PublicKeySize = 32;

        public ushort otherPlayerId;
        public string sessionToken;
        /// <summary>
        /// X25519 ephemeral public key of the sender, relayed by the server so the two
        /// peers can derive a per-pair key and always encrypt the direct (P2P) link.
        /// </summary>
        public byte[] ephemeralPublicKey;

        public void Deserialize(NetDataReader reader)
        {
            otherPlayerId = reader.GetUShort();
            sessionToken = reader.GetString(MaxTokenLength);
            byte hasKey = reader.GetByte();
            if (hasKey == 1 && reader.AvailableBytes >= PublicKeySize)
            {
                ephemeralPublicKey = new byte[PublicKeySize];
                reader.GetBytes(ephemeralPublicKey, PublicKeySize);
            }
            else
            {
                ephemeralPublicKey = null;
            }
        }

        public void Serialize(NetDataWriter writer)
        {
            writer.Put(otherPlayerId);
            writer.Put(sessionToken ?? string.Empty, MaxTokenLength);
            if (ephemeralPublicKey != null && ephemeralPublicKey.Length == PublicKeySize)
            {
                writer.Put((byte)1);
                writer.Put(ephemeralPublicKey);
            }
            else
            {
                writer.Put((byte)0);
            }
        }
    }

    /// <summary>
    /// Client → server on the P2P channel under P2PSub_IntroduceRequest: this side's transport
    /// address (the iroh endpoint address as JSON) for one session.
    /// Wire: [sessionToken:string][addr:bytesWithLength]
    /// </summary>
    public struct BasisP2PIntroduceRequest
    {
        public string sessionToken;
        public byte[] endpointAddr;

        public void Deserialize(NetDataReader reader)
        {
            sessionToken = reader.GetString(BasisP2PSignalMessage.MaxTokenLength);
            endpointAddr = reader.GetBytesWithLength();
        }

        public void Serialize(NetDataWriter writer)
        {
            writer.Put(sessionToken ?? string.Empty, BasisP2PSignalMessage.MaxTokenLength);
            writer.PutBytesWithLength(endpointAddr ?? System.Array.Empty<byte>());
        }
    }

    /// <summary>
    /// Server → client on the P2P channel under P2PSub_Introduce: the other side's endpoint
    /// address for this session, plus which of the two the receiver is (so exactly one side dials).
    /// Wire: [sessionToken:string][otherPlayerId:ushort][dial:1][addr:bytesWithLength]
    /// </summary>
    public struct BasisP2PIntroduce
    {
        public string sessionToken;
        public ushort otherPlayerId;
        /// <summary>True for the side that should open the connection; the other side accepts.</summary>
        public bool dial;
        public byte[] endpointAddr;

        public void Deserialize(NetDataReader reader)
        {
            sessionToken = reader.GetString(BasisP2PSignalMessage.MaxTokenLength);
            otherPlayerId = reader.GetUShort();
            dial = reader.GetBool();
            endpointAddr = reader.GetBytesWithLength();
        }

        public void Serialize(NetDataWriter writer)
        {
            writer.Put(sessionToken ?? string.Empty, BasisP2PSignalMessage.MaxTokenLength);
            writer.Put(otherPlayerId);
            writer.Put(dial);
            writer.PutBytesWithLength(endpointAddr ?? System.Array.Empty<byte>());
        }
    }
}
