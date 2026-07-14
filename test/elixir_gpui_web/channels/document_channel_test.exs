defmodule ElixirGpuiWeb.DocumentChannelTest do
  use ElixirGpuiWeb.ChannelCase, async: false

  alias ElixirGpuiWeb.{DocumentChannel, UserSocket}

  test "synchronizes a Yex update with another channel subscriber" do
    topic = "documents:test-#{System.unique_integer([:positive])}"

    {:ok, _, first} =
      UserSocket
      |> socket("first", %{})
      |> subscribe_and_join(DocumentChannel, topic)

    {:ok, _, _second} =
      UserSocket
      |> socket("second", %{})
      |> subscribe_and_join(DocumentChannel, topic)

    document = Yex.Doc.new()
    text = Yex.Doc.get_text(document, "content")
    :ok = Yex.Text.insert(text, 0, "shared text")
    update = Yex.encode_state_as_update!(document)
    message = Yex.Sync.message_encode!({:sync, {:sync_update, update}})

    ref = push(first, "yjs", %{"message" => Base.encode64(message)})

    refute_reply ref, :error
    assert_push "yjs", %{message: encoded}
    assert {:ok, broadcast} = Base.decode64(encoded)
    assert {:ok, {:sync, {:sync_update, _update}}} = Yex.Sync.message_decode(broadcast)
  end

  test "rejects malformed document ids and sync messages" do
    assert {:error, %{reason: "invalid document id"}} =
             UserSocket
             |> socket("invalid", %{})
             |> subscribe_and_join(DocumentChannel, "documents:not/valid")

    {:ok, _, socket} =
      UserSocket
      |> socket("valid", %{})
      |> subscribe_and_join(DocumentChannel, "documents:valid")

    ref = push(socket, "yjs", %{"message" => "not base64"})
    assert_reply ref, :error, %{reason: "invalid sync message"}
  end
end
