defmodule ElixirGpuiWeb.DocumentChannel do
  use ElixirGpuiWeb, :channel

  alias ElixirGpui.Collaboration.{DocumentCatalog, RoomServer}

  @max_encoded_message_size 1_400_000
  @max_message_size 1_048_576
  @document_id ~r/\A[a-zA-Z0-9_-]{1,64}\z/

  @impl true
  def join("documents:" <> document_id, %{"client_id" => encoded_client_id} = payload, socket)
      when is_binary(encoded_client_id) do
    case Integer.parse(encoded_client_id) do
      {client_id, ""} when client_id >= 0 ->
        join_document(document_id, client_id, Map.get(payload, "documents", []), socket)

      _ ->
        {:error, %{reason: "invalid client id"}}
    end
  end

  def join("documents:" <> _document_id, _payload, _socket) do
    {:error, %{reason: "invalid client id"}}
  end

  @impl true
  def handle_in("awareness", %{"message" => encoded}, socket)
      when is_binary(encoded) and byte_size(encoded) <= @max_encoded_message_size do
    with {:ok, message} <- Base.decode64(encoded),
         true <- byte_size(message) <= @max_message_size do
      broadcast_from!(socket, "awareness", %{message: encoded})
      {:noreply, socket}
    else
      _ -> {:reply, {:error, %{reason: "invalid awareness message"}}, socket}
    end
  end

  def handle_in("awareness", _payload, socket) do
    {:reply, {:error, %{reason: "invalid awareness message"}}, socket}
  end

  @impl true
  def handle_in("yjs", %{"message" => encoded}, socket)
      when is_binary(encoded) and byte_size(encoded) <= @max_encoded_message_size do
    with {:ok, message} <- Base.decode64(encoded),
         true <- byte_size(message) <= @max_message_size,
         result <- RoomServer.process_message_v1(socket.assigns.room, message, self()),
         :ok <- push_replies(result, socket) do
      {:noreply, socket}
    else
      _ -> {:reply, {:error, %{reason: "invalid sync message"}}, socket}
    end
  end

  def handle_in("yjs", _payload, socket) do
    {:reply, {:error, %{reason: "invalid sync message"}}, socket}
  end

  @impl true
  def handle_info({:documents_updated, documents}, socket) do
    push(socket, "documents", %{documents: documents})
    {:noreply, socket}
  end

  def handle_info(:push_documents, socket) do
    push(socket, "documents", %{documents: DocumentCatalog.list()})
    {:noreply, socket}
  end

  def handle_info({:DOWN, _ref, :process, room, _reason}, %{assigns: %{room: room}} = socket) do
    {:stop, :document_unavailable, socket}
  end

  def handle_info(_message, socket), do: {:noreply, socket}

  @impl true
  def terminate(_reason, %{assigns: %{client_id: client_id}} = socket) do
    ElixirGpuiWeb.Endpoint.broadcast_from(
      self(),
      socket.topic,
      "awareness_leave",
      %{client_id: Integer.to_string(client_id)}
    )

    :ok
  end

  def terminate(_reason, _socket), do: :ok

  defp join_document(document_id, client_id, documents, socket) do
    with true <- Regex.match?(@document_id, document_id),
         {:ok, room} <- RoomServer.ensure_started(document_id) do
      :ok = DocumentCatalog.merge(documents)
      :ok = DocumentCatalog.subscribe()
      send(self(), :push_documents)
      Process.monitor(room)
      {:ok, assign(socket, room: room, document_id: document_id, client_id: client_id)}
    else
      false -> {:error, %{reason: "invalid document id"}}
      {:error, _reason} -> {:error, %{reason: "document unavailable"}}
    end
  end

  defp push_replies({:ok, replies}, socket) do
    Enum.each(replies, fn reply ->
      push(socket, "yjs", %{message: Base.encode64(reply)})
    end)

    :ok
  end

  defp push_replies(:ok, _socket), do: :ok
  defp push_replies({:error, _reason}, _socket), do: :error
end
