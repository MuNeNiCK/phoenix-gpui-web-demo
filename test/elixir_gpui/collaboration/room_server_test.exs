defmodule ElixirGpui.Collaboration.RoomServerTest do
  use ExUnit.Case, async: false

  alias ElixirGpui.Collaboration.RoomServer

  test "seeds the README room with introductory Markdown" do
    assert {:ok, room} = RoomServer.ensure_started("readme")

    content =
      room
      |> :sys.get_state()
      |> Map.fetch!(:doc)
      |> Yex.Doc.get_text("content")
      |> Yex.Text.to_string()

    assert content =~ "# Elixir GPUI Workspace"
    assert content =~ "collaborative Markdown editor"
  end
end
