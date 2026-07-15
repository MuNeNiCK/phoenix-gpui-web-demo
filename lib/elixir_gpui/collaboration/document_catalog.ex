defmodule ElixirGpui.Collaboration.DocumentCatalog do
  @moduledoc false

  use Agent

  @topic "workspace:documents"
  @document_id ~r/\A[a-zA-Z0-9_-]{1,64}\z/
  @default_documents [%{"id" => "readme", "title" => "README.md"}]

  def start_link(_options) do
    Agent.start_link(
      fn -> %{documents: index(@default_documents), deleted: MapSet.new()} end,
      name: __MODULE__
    )
  end

  def list do
    Agent.get(__MODULE__, fn state ->
      state.documents
      |> Map.values()
      |> Enum.sort_by(&String.downcase(&1["title"]))
    end)
  end

  def merge(documents) when is_list(documents) do
    valid_documents = Enum.filter(documents, &valid?/1)

    changed? =
      Agent.get_and_update(__MODULE__, fn state ->
        additions =
          valid_documents
          |> Enum.reject(&MapSet.member?(state.deleted, &1["id"]))
          |> index()

        documents = Map.merge(state.documents, additions)
        {documents != state.documents, %{state | documents: documents}}
      end)

    if changed? do
      Phoenix.PubSub.broadcast(ElixirGpui.PubSub, @topic, {:documents_updated, list()})
    end

    :ok
  end

  def merge(_documents), do: :ok

  def delete("readme"), do: {:error, :protected_document}

  def delete(document_id) when is_binary(document_id) do
    changed? =
      Agent.get_and_update(__MODULE__, fn state ->
        changed? = Map.has_key?(state.documents, document_id)

        updated = %{
          state
          | documents: Map.delete(state.documents, document_id),
            deleted: MapSet.put(state.deleted, document_id)
        }

        {changed?, updated}
      end)

    if changed? do
      Phoenix.PubSub.broadcast(ElixirGpui.PubSub, @topic, {:documents_updated, list()})
    end

    :ok
  end

  def deleted?(document_id) do
    Agent.get(__MODULE__, &MapSet.member?(&1.deleted, document_id))
  end

  def subscribe do
    Phoenix.PubSub.subscribe(ElixirGpui.PubSub, @topic)
  end

  defp valid?(%{"id" => id, "title" => title}) do
    is_binary(id) and Regex.match?(@document_id, id) and is_binary(title) and
      String.trim(title) != "" and byte_size(title) <= 256
  end

  defp valid?(_document), do: false

  defp index(documents), do: Map.new(documents, &{&1["id"], &1})
end
