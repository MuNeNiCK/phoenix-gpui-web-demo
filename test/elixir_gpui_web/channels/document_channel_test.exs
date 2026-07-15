defmodule ElixirGpuiWeb.DocumentChannelTest do
  use ElixirGpuiWeb.ChannelCase, async: false

  alias ElixirGpuiWeb.{DocumentChannel, UserSocket}

  test "synchronizes a Yex update with another channel subscriber" do
    topic = "documents:test-#{System.unique_integer([:positive])}"

    {:ok, _, first} =
      UserSocket
      |> socket("first", %{})
      |> subscribe_and_join(DocumentChannel, topic, %{"client_id" => "1"})

    {:ok, _, _second} =
      UserSocket
      |> socket("second", %{})
      |> subscribe_and_join(DocumentChannel, topic, %{"client_id" => "2"})

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
    assert {:error, %{reason: "invalid client id"}} =
             UserSocket
             |> socket("invalid-client", %{})
             |> subscribe_and_join(DocumentChannel, "documents:valid", %{"client_id" => "NaN"})

    assert {:error, %{reason: "invalid document id"}} =
             UserSocket
             |> socket("invalid", %{})
             |> subscribe_and_join(DocumentChannel, "documents:not/valid", %{"client_id" => "1"})

    {:ok, _, socket} =
      UserSocket
      |> socket("valid", %{})
      |> subscribe_and_join(DocumentChannel, "documents:valid", %{"client_id" => "1"})

    ref = push(socket, "yjs", %{"message" => "not base64"})
    assert_reply ref, :error, %{reason: "invalid sync message"}
  end

  test "broadcasts awareness updates and removes disconnected clients" do
    topic = "documents:awareness-#{System.unique_integer([:positive])}"

    {:ok, _, first} =
      UserSocket
      |> socket("first", %{})
      |> subscribe_and_join(DocumentChannel, topic, %{"client_id" => "11"})

    {:ok, _, _second} =
      UserSocket
      |> socket("second", %{})
      |> subscribe_and_join(DocumentChannel, topic, %{"client_id" => "22"})

    message = Base.encode64(<<1, 2, 3>>)
    ref = push(first, "awareness", %{"message" => message})

    refute_reply ref, :error
    assert_push "awareness", %{message: ^message}

    Process.unlink(first.channel_pid)
    :ok = close(first)
    assert_push "awareness_leave", %{client_id: "11"}
  end

  test "shares created documents with connected clients" do
    suffix = System.unique_integer([:positive])
    document_id = "created-#{suffix}"
    document = %{"id" => document_id, "title" => "Created #{suffix}.md"}

    {:ok, _, _first} =
      UserSocket
      |> socket("catalog-first", %{})
      |> subscribe_and_join(DocumentChannel, "documents:shared-notes", %{
        "client_id" => "31"
      })

    assert_push "documents", %{documents: initial_documents}
    refute Enum.any?(initial_documents, &(&1["id"] == document_id))

    {:ok, _, second} =
      UserSocket
      |> socket("catalog-second", %{})
      |> subscribe_and_join(DocumentChannel, "documents:#{document_id}", %{
        "client_id" => "32",
        "documents" => [document]
      })

    assert_push "documents", %{documents: documents}
    assert Enum.any?(documents, &(&1 == document))
    assert_push "documents", %{documents: documents}
    assert Enum.any?(documents, &(&1 == document))

    ref = push(second, "delete_document", %{"document_id" => document_id})
    refute_reply ref, :error
    assert_push "documents", %{documents: documents}
    refute Enum.any?(documents, &(&1["id"] == document_id))

    assert {:error, %{reason: "document deleted"}} =
             UserSocket
             |> socket("deleted-document", %{})
             |> subscribe_and_join(DocumentChannel, "documents:#{document_id}", %{
               "client_id" => "33",
               "documents" => [document]
             })

    ref = push(second, "delete_document", %{"document_id" => "readme"})
    assert_reply ref, :error, %{reason: "protected document"}
  end
end
