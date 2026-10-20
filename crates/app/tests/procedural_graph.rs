use subcraft_app::{Command, NewLayer, Session};
use subcraft_core::{ChannelKind, LayerId};
use subcraft_doc::{Layer, LayerContent, ProceduralGraph};
use subcraft_image::TiledImage;
use subcraft_mesh::primitives;

fn session() -> Session {
    Session::new(primitives::cube(2.0), 64)
}

fn add_ao_source(session: &mut Session) -> LayerId {
    let mut source = Layer::new_paint("AO source", session.selected_texture_set());
    let LayerContent::Paint(paint) = &mut source.content else {
        unreachable!()
    };
    paint.channels.insert(
        ChannelKind::AmbientOcclusion,
        TiledImage::new(64, 64, 2, [0.5, 0.0, 0.0, 0.0]),
    );
    let id = source.id;
    session
        .execute(Command::InsertLayer {
            layer: Box::new(source),
            label: "Add AO source".into(),
        })
        .unwrap();
    id
}

#[test]
fn roughness_graph_add_edit_undo_and_redo_use_document_history() {
    let mut session = session();
    let layer = session.selected_layer().unwrap().id;
    let before = session.history().entries().len();
    let graph = ProceduralGraph::value_noise(17, 4.0);
    session
        .execute(Command::SetProceduralGraph {
            layer,
            graph: Some(graph.clone()),
        })
        .unwrap();
    assert_eq!(session.history().entries().len(), before + 1);
    let LayerContent::Paint(paint) = &session.selected_texture_set().find(layer).unwrap().content
    else {
        panic!("not a paint layer")
    };
    assert_eq!(paint.procedural_graph, Some(graph.clone()));

    session.execute(Command::Undo).unwrap();
    let LayerContent::Paint(paint) = &session.selected_texture_set().find(layer).unwrap().content
    else {
        panic!("not a paint layer")
    };
    assert!(paint.procedural_graph.is_none());
    session.execute(Command::Redo).unwrap();
    let LayerContent::Paint(paint) = &session.selected_texture_set().find(layer).unwrap().content
    else {
        panic!("not a paint layer")
    };
    assert_eq!(
        paint
            .procedural_graph
            .as_ref()
            .unwrap()
            .compile()
            .unwrap()
            .evaluate([0.25, 0.75], 1.0),
        graph.compile().unwrap().evaluate([0.25, 0.75], 1.0)
    );
}

#[test]
fn ao_source_is_a_same_set_paint_layer_and_can_be_undone() {
    let mut session = session();
    let target = session.selected_layer().unwrap().id;
    let source = add_ao_source(&mut session);
    session
        .execute(Command::SetProceduralGraph {
            layer: target,
            graph: Some(ProceduralGraph::value_noise_times_occlusion(
                8, 12.0, source,
            )),
        })
        .unwrap();
    let LayerContent::Paint(paint) = &session.selected_texture_set().find(target).unwrap().content
    else {
        panic!("not a paint layer")
    };
    assert_eq!(
        paint
            .procedural_graph
            .as_ref()
            .unwrap()
            .occlusion_source_layer_id(),
        Some(source)
    );
    session.execute(Command::Undo).unwrap();
    let LayerContent::Paint(paint) = &session.selected_texture_set().find(target).unwrap().content
    else {
        panic!("not a paint layer")
    };
    assert!(paint.procedural_graph.is_none());
}

#[test]
fn invalid_graph_parameters_and_ao_references_are_rejected_without_history_mutation() {
    let mut session = session();
    let layer = session.selected_layer().unwrap().id;
    let before = session.document().clone();
    let history = session.history().entries().len();

    for graph in [
        ProceduralGraph::value_noise(1, f32::NAN),
        ProceduralGraph::value_noise_times_occlusion(1, 2.0, LayerId::nil()),
        ProceduralGraph::value_noise_times_occlusion(1, 2.0, LayerId::new()),
        ProceduralGraph::value_noise_times_occlusion(1, 2.0, layer),
    ] {
        assert!(
            session
                .execute(Command::SetProceduralGraph {
                    layer,
                    graph: Some(graph)
                })
                .is_err()
        );
        assert_eq!(session.document(), &before);
        assert_eq!(session.history().entries().len(), history);
    }

    session.execute(Command::AddLayer(NewLayer::Fill)).unwrap();
    let fill = session.selected_layer().unwrap().id;
    let history = session.history().entries().len();
    assert!(
        session
            .execute(Command::SetProceduralGraph {
                layer: fill,
                graph: Some(ProceduralGraph::value_noise(1, 2.0))
            })
            .is_err()
    );
    assert_eq!(session.history().entries().len(), history);
}
