defmodule ElixirGpui.Collaboration.RoomServer do
  @moduledoc false

  use Yex.DocServer, restart: :transient

  alias Yex.Sync

  @registry ElixirGpui.Collaboration.Registry
  @supervisor ElixirGpui.Collaboration.Supervisor

  def start_room_link(document_id) do
    start_link([document_id: document_id], name: via(document_id))
  end

  def child_spec(document_id) do
    %{
      id: {__MODULE__, document_id},
      start: {__MODULE__, :start_room_link, [document_id]},
      restart: :transient
    }
  end

  def ensure_started(document_id) do
    case Registry.lookup(@registry, document_id) do
      [{pid, _value}] ->
        {:ok, pid}

      [] ->
        case DynamicSupervisor.start_child(@supervisor, {__MODULE__, document_id}) do
          {:ok, pid} -> {:ok, pid}
          {:error, {:already_started, pid}} -> {:ok, pid}
          {:error, reason} -> {:error, reason}
        end
    end
  end

  @impl true
  def init(options, state) do
    document_id = Keyword.fetch!(options, :document_id)
    {:ok, assign(state, document_id: document_id, topic: "documents:#{document_id}")}
  end

  @impl true
  def handle_update_v1(_doc, update, origin, state) do
    with {:ok, sync_update} <- Sync.get_update(update),
         {:ok, message} <- Sync.message_encode({:sync, sync_update}) do
      payload = %{message: Base.encode64(message)}

      if is_pid(origin) do
        ElixirGpuiWeb.Endpoint.broadcast_from(origin, state.assigns.topic, "yjs", payload)
      else
        ElixirGpuiWeb.Endpoint.broadcast(state.assigns.topic, "yjs", payload)
      end
    end

    {:noreply, state}
  end

  defp via(document_id), do: {:via, Registry, {@registry, document_id}}
end
