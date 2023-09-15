use crate::error::Result;
use crate::planning::stitch::CommPlan;
use crate::proto::gen::tasks::{Variable, VariableNamespace};
use crate::spec::axis::{AxisFormatTypeSpec, AxisSpec};
use crate::spec::chart::{ChartSpec, ChartVisitor, MutChartVisitor};
use crate::spec::data::DataSpec;
use crate::spec::mark::{MarkEncodingField, MarkSpec};
use crate::spec::scale::{
    ScaleDataReferenceOrSignalSpec, ScaleDomainSpec, ScaleSpec, ScaleTypeSpec,
};
use crate::spec::transform::formula::FormulaTransformSpec;
use crate::spec::transform::TransformSpec;
use crate::task_graph::graph::ScopedVariable;
use crate::task_graph::scope::TaskScope;
use itertools::sorted;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

/// This planning phase converts select datetime columns from the default millisecond UTC
/// representation to naive datetime strings in an "output timezone". This is only done for datetime
/// columns that are scaled using a (non-utc) `time` scale in the client specification.
///
/// This is needed in order for the chart displayed by the client to be consistent regardless of
/// the browser's local timezone.  Viewers from all timezones should see the chart displayed as
/// it would look when generated by pure Vega in the `output_tz` timezone.
pub fn stringify_local_datetimes(
    server_spec: &mut ChartSpec,
    client_spec: &mut ChartSpec,
    comm_plan: &CommPlan,
    domain_dataset_fields: &HashMap<ScopedVariable, (ScopedVariable, String)>,
) -> Result<()> {
    // Build task scope for client spec
    let client_scope = client_spec.to_task_scope()?;

    // Collect the name/scope of all time scales
    let mut visitor = CollectScalesTypesVisitor::new();
    client_spec.walk(&mut visitor)?;
    let local_time_scales = visitor.local_time_scales;

    // Gather candidate datasets
    let server_to_client_datasets: HashSet<_> = comm_plan
        .server_to_client
        .iter()
        .cloned()
        .filter(|var| var.0.namespace == VariableNamespace::Data as i32)
        .collect();

    let mut visitor = CollectCandidateDatasetMapping::new(
        client_scope.clone(),
        server_to_client_datasets.clone(),
    );
    client_spec.walk(&mut visitor)?;
    let candidate_dataset_mapping = visitor.candidate_dataset_mapping;

    // Collect data fields to convert to datetime strings
    let mut visitor = CollectLocalTimeScaledFieldsVisitor::new(
        client_scope,
        local_time_scales,
        candidate_dataset_mapping,
    );
    client_spec.walk(&mut visitor)?;

    // Collect fields that are produced in datasets
    let mut visitor = CollectLocalTimeDataFieldsVisitor::try_new(
        visitor.local_datetime_fields,
        &server_to_client_datasets,
        server_spec,
    )?;
    server_spec.walk(&mut visitor)?;
    let local_datetime_fields = visitor.local_datetime_fields;

    // Add formula transforms to server spec
    let server_scope = server_spec.to_task_scope()?;
    let mut visitor = StringifyLocalDatetimeFieldsVisitor::new(
        &local_datetime_fields,
        &server_scope,
        domain_dataset_fields,
    );
    server_spec.walk_mut(&mut visitor)?;

    // Add format spec to client spec (to parse strings as local dates)
    let mut visitor =
        FormatLocalDatetimeFieldsVisitor::new(&local_datetime_fields, domain_dataset_fields);
    client_spec.walk_mut(&mut visitor)?;

    Ok(())
}

/// Visitor to collect the non-UTC time scales
struct CollectScalesTypesVisitor {
    pub local_time_scales: HashSet<ScopedVariable>,
}

impl CollectScalesTypesVisitor {
    pub fn new() -> Self {
        Self {
            local_time_scales: Default::default(),
        }
    }
}

impl ChartVisitor for CollectScalesTypesVisitor {
    fn visit_scale(&mut self, scale: &ScaleSpec, scope: &[u32]) -> Result<()> {
        let var = (Variable::new_scale(&scale.name), Vec::from(scope));
        if matches!(scale.type_, Some(ScaleTypeSpec::Time)) {
            self.local_time_scales.insert(var);
        }

        Ok(())
    }

    fn visit_axis(&mut self, axis: &AxisSpec, scope: &[u32]) -> Result<()> {
        if matches!(axis.format_type, Some(AxisFormatTypeSpec::Time)) {
            let var = (Variable::new_scale(&axis.scale), Vec::from(scope));
            self.local_time_scales.insert(var);
        }
        Ok(())
    }
}

/// Visitor to collect mapping from mark dataset to server_to_client dataset
/// (following facet aliasing)
struct CollectCandidateDatasetMapping {
    pub scope: TaskScope,
    pub server_to_client_datasets: HashSet<ScopedVariable>,
    pub candidate_dataset_mapping: HashMap<ScopedVariable, ScopedVariable>,
}

impl CollectCandidateDatasetMapping {
    pub fn new(scope: TaskScope, server_to_client_datasets: HashSet<ScopedVariable>) -> Self {
        // Initialize candidate_dataset_mapping with all server_to_client datasets
        let candidate_dataset_mapping: HashMap<_, _> = server_to_client_datasets
            .iter()
            .map(|var| (var.clone(), var.clone()))
            .collect();
        Self {
            scope,
            server_to_client_datasets,
            candidate_dataset_mapping,
        }
    }
}

impl ChartVisitor for CollectCandidateDatasetMapping {
    fn visit_group_mark(&mut self, mark: &MarkSpec, scope: &[u32]) -> Result<()> {
        // Add to candidate_dataset_mapping facet datasets that reference a server_to_scale dataset
        if let Some(from) = &mark.from {
            if let Some(facet) = &from.facet {
                let data_var = Variable::new_data(&facet.data);
                let resolved_data = self.scope.resolve_scope(&data_var, scope)?;
                let resolved_data_scoped: ScopedVariable =
                    (resolved_data.var.clone(), resolved_data.scope);

                if self
                    .server_to_client_datasets
                    .contains(&resolved_data_scoped)
                {
                    let facet_var = Variable::new_data(&facet.name);
                    let facet_scoped_var: ScopedVariable = (facet_var, Vec::from(scope));
                    self.candidate_dataset_mapping
                        .insert(facet_scoped_var, resolved_data_scoped);
                }
            }
        }

        Ok(())
    }
}

/// Visitor to collect data fields that are scaled by a non-UTC time scale
struct CollectLocalTimeScaledFieldsVisitor {
    pub scope: TaskScope,
    pub candidate_dataset_mapping: HashMap<ScopedVariable, ScopedVariable>,
    pub local_time_scales: HashSet<ScopedVariable>,
    pub local_datetime_fields: HashMap<ScopedVariable, HashSet<String>>,
}

impl CollectLocalTimeScaledFieldsVisitor {
    pub fn new(
        scope: TaskScope,
        local_time_scales: HashSet<ScopedVariable>,
        candidate_dataset_mapping: HashMap<ScopedVariable, ScopedVariable>,
    ) -> Self {
        Self {
            scope,
            candidate_dataset_mapping,
            local_time_scales,
            local_datetime_fields: Default::default(),
        }
    }
}

impl ChartVisitor for CollectLocalTimeScaledFieldsVisitor {
    fn visit_non_group_mark(&mut self, mark: &MarkSpec, scope: &[u32]) -> Result<()> {
        if let Some(mark_from) = &mark.from {
            if let Some(mark_data) = &mark_from.data {
                let mark_data_var = Variable::new_data(mark_data);
                let resolved_mark_data = self.scope.resolve_scope(&mark_data_var, scope)?;
                let resolved_mark_data_scoped =
                    (resolved_mark_data.var.clone(), resolved_mark_data.scope);
                if let Some(server_to_client_data_var) = self
                    .candidate_dataset_mapping
                    .get(&resolved_mark_data_scoped)
                {
                    // We've found a mark with a dataset that is eligible for date string
                    // conversion
                    if let Some(encode) = &mark.encode {
                        for (_, encodings) in encode.encodings.iter() {
                            for (_, channels) in encodings.channels.iter() {
                                for channel in channels.to_vec() {
                                    if let (Some(scale), Some(MarkEncodingField::Field(field))) =
                                        (&channel.scale, &channel.field)
                                    {
                                        let scale_var = Variable::new_scale(scale);
                                        let resolved_scale =
                                            self.scope.resolve_scope(&scale_var, scope)?;
                                        let resolved_scoped_scale = (
                                            resolved_scale.var.clone(),
                                            resolved_scale.scope.clone(),
                                        );

                                        if self.local_time_scales.contains(&resolved_scoped_scale) {
                                            // Save off field for dataset
                                            let entry = self
                                                .local_datetime_fields
                                                .entry(server_to_client_data_var.clone());
                                            let fields = entry.or_default();
                                            fields.insert(field.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn visit_scale(&mut self, scale: &ScaleSpec, scope: &[u32]) -> Result<()> {
        let scale_var: ScopedVariable = (Variable::new_scale(&scale.name), Vec::from(scope));
        if self.local_time_scales.contains(&scale_var) {
            if let Some(domain) = &scale.domain {
                let field_refs = match domain {
                    ScaleDomainSpec::FieldReference(field_ref) => {
                        vec![field_ref.clone()]
                    }
                    ScaleDomainSpec::FieldsReference(fields_ref) => {
                        fields_ref.to_field_references()
                    }
                    ScaleDomainSpec::FieldsReferences(fields_ref) => fields_ref
                        .fields
                        .iter()
                        .filter_map(|f| {
                            if let ScaleDataReferenceOrSignalSpec::Reference(f) = f {
                                Some(f.clone())
                            } else {
                                None
                            }
                        })
                        .collect(),
                    _ => Default::default(),
                };
                for field_ref in &field_refs {
                    let data_var = Variable::new_data(&field_ref.data);
                    if let Ok(resolved) = self.scope.resolve_scope(&data_var, scope) {
                        let scoped_data_var = (resolved.var, resolved.scope);
                        if self
                            .candidate_dataset_mapping
                            .contains_key(&scoped_data_var)
                        {
                            // Save off field for dataset
                            let entry = self.local_datetime_fields.entry(scoped_data_var.clone());
                            let fields = entry.or_default();
                            fields.insert(field_ref.field.clone());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn get_local_datetime_fields(
    data_var: &ScopedVariable,
    local_datetime_fields: &HashMap<ScopedVariable, HashSet<String>>,
    domain_dataset_fields: &HashMap<ScopedVariable, (ScopedVariable, String)>,
) -> HashSet<String> {
    // Map dataset variable
    if let Some(fields) = local_datetime_fields.get(data_var) {
        fields.clone()
    } else if let Some((mapped_var, field)) = domain_dataset_fields.get(data_var) {
        if let Some(fields) = local_datetime_fields.get(mapped_var) {
            if fields.contains(field) {
                vec![field.clone()].into_iter().collect()
            } else {
                Default::default()
            }
        } else {
            Default::default()
        }
    } else {
        Default::default()
    }
}

/// Visitor to collect local datetime columns produced by datasets
struct CollectLocalTimeDataFieldsVisitor<'a> {
    pub local_datetime_fields: HashMap<ScopedVariable, HashSet<String>>,
    pub server_to_client_datasets: &'a HashSet<ScopedVariable>,
    pub chart_spec: &'a ChartSpec,
    pub task_scope: TaskScope,
}

impl<'a> CollectLocalTimeDataFieldsVisitor<'a> {
    pub fn try_new(
        local_datetime_fields: HashMap<ScopedVariable, HashSet<String>>,
        server_to_client_datasets: &'a HashSet<ScopedVariable>,
        chart_spec: &'a ChartSpec,
    ) -> Result<Self> {
        Ok(Self {
            local_datetime_fields,
            server_to_client_datasets,
            chart_spec,
            task_scope: chart_spec.to_task_scope()?,
        })
    }
}

impl<'a> ChartVisitor for CollectLocalTimeDataFieldsVisitor<'a> {
    fn visit_data(&mut self, data: &DataSpec, scope: &[u32]) -> Result<()> {
        // Add local datetime columns produced by the dataset.
        // Note: This isn't quite complete, for derived datasets input_local_datetime_columns
        //       should be computed from the parent dataset, but we won't be able to do this
        //       in a visitor
        let local_columns =
            data.local_datetime_columns_produced(self.chart_spec, &self.task_scope, scope)?;
        let dataset_var: ScopedVariable = (Variable::new_data(&data.name), Vec::from(scope));
        if self.server_to_client_datasets.contains(&dataset_var) {
            match self.local_datetime_fields.entry(dataset_var) {
                Entry::Occupied(mut v) => {
                    v.get_mut().extend(local_columns);
                }
                Entry::Vacant(v) => {
                    v.insert(local_columns.into_iter().collect());
                }
            }
        }
        Ok(())
    }
}

/// Visitor to stringify select datetime fields
struct StringifyLocalDatetimeFieldsVisitor<'a> {
    pub local_datetime_fields: &'a HashMap<ScopedVariable, HashSet<String>>,
    pub scope: &'a TaskScope,
    pub domain_dataset_fields: &'a HashMap<ScopedVariable, (ScopedVariable, String)>,
}

impl<'a> StringifyLocalDatetimeFieldsVisitor<'a> {
    pub fn new(
        local_datetime_fields: &'a HashMap<ScopedVariable, HashSet<String>>,
        scope: &'a TaskScope,
        domain_dataset_fields: &'a HashMap<ScopedVariable, (ScopedVariable, String)>,
    ) -> Self {
        Self {
            local_datetime_fields,
            scope,
            domain_dataset_fields,
        }
    }
}

impl<'a> MutChartVisitor for StringifyLocalDatetimeFieldsVisitor<'a> {
    fn visit_data(&mut self, data: &mut DataSpec, scope: &[u32]) -> Result<()> {
        let data_var = (Variable::new_data(&data.name), Vec::from(scope));

        // Map dataset variable
        let fields = get_local_datetime_fields(
            &data_var,
            self.local_datetime_fields,
            self.domain_dataset_fields,
        );

        for field in sorted(fields) {
            let expr_str =
                format!("timeFormat(toDate(datum['{field}'], 'local'), '%Y-%m-%dT%H:%M:%S.%L')");

            let transforms = &mut data.transform;
            let transform = FormulaTransformSpec {
                expr: expr_str,
                as_: field.to_string(),
                extra: Default::default(),
            };
            transforms.push(TransformSpec::Formula(transform))
        }

        // Check if dataset is a child a stringified dataset. If so, we need to convert
        // datetime strings back to the utc millisecond representation
        if let Some(source) = &data.source {
            let source_var = Variable::new_data(source);
            let source_resolved = self.scope.resolve_scope(&source_var, scope)?;
            let source_resolved_var = (source_resolved.var, source_resolved.scope);
            if let Some(fields) = self.local_datetime_fields.get(&source_resolved_var) {
                for field in sorted(fields) {
                    let expr_str = format!("toDate(datum['{field}'], 'local')");
                    let transforms = &mut data.transform;
                    let transform = FormulaTransformSpec {
                        expr: expr_str,
                        as_: field.to_string(),
                        extra: Default::default(),
                    };
                    transforms.insert(0, TransformSpec::Formula(transform))
                }
            }
        }

        Ok(())
    }
}

/// Visitor to add format parse specification for local dates
struct FormatLocalDatetimeFieldsVisitor<'a> {
    pub local_datetime_fields: &'a HashMap<ScopedVariable, HashSet<String>>,
    pub domain_dataset_fields: &'a HashMap<ScopedVariable, (ScopedVariable, String)>,
}

impl<'a> FormatLocalDatetimeFieldsVisitor<'a> {
    pub fn new(
        local_datetime_fields: &'a HashMap<ScopedVariable, HashSet<String>>,
        domain_dataset_fields: &'a HashMap<ScopedVariable, (ScopedVariable, String)>,
    ) -> Self {
        Self {
            local_datetime_fields,
            domain_dataset_fields,
        }
    }
}

impl<'a> MutChartVisitor for FormatLocalDatetimeFieldsVisitor<'a> {
    fn visit_data(&mut self, data: &mut DataSpec, scope: &[u32]) -> Result<()> {
        let data_var = (Variable::new_data(&data.name), Vec::from(scope));
        let fields = get_local_datetime_fields(
            &data_var,
            self.local_datetime_fields,
            self.domain_dataset_fields,
        );
        for field in sorted(fields) {
            let transforms = &mut data.transform;
            let transform = FormulaTransformSpec {
                expr: format!("toDate(datum['{field}'])"),
                as_: field.to_string(),
                extra: Default::default(),
            };
            transforms.insert(0, TransformSpec::Formula(transform))
        }

        Ok(())
    }
}
